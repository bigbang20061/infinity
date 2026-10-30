//! Infinity emulator server: proxy and sol node, gateway, chat, image server and CDN.
//! The handshake mirrors the client's macho networking; state comes from the game
//! database and SDE. Interrupted reloads return to cargo before workers or listeners
//! start. On SIGINT or SIGTERM, listeners stop and changed pilot positions are flushed
//! with a bounded wait; existing sessions are not drained.
//! Authenticated account ownership is process-local; replacements wait for
//! prior cleanup. Cross-process session coordination is not supported.

mod cdn;
mod retail;
mod chat;
mod client;
mod conn;
mod consts;
mod dao;
mod db;
mod framing;
mod gateway;
mod handshake;
mod images;
mod live_sessions;
mod macho;
mod messages;
mod services;
mod session;
mod space;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Server allocation policy: initial ephemeral client ID.
const FIRST_CLIENT_ID: i64 = 1_000_000_000_000;
/// Server online population counter delta when a session joins.
const ONLINE_JOIN_DELTA: i64 = 1;
/// Server online population counter delta when a session leaves.
const ONLINE_LEAVE_DELTA: i64 = -1;
/// Server policy: leave capacity for administrators and tools under PostgreSQL's default 100 connections.
const DATABASE_POOL_SIZE_DEFAULT: usize = 16;
/// Server design decision: bound the shutdown position flush to ten seconds.
const POSITION_FLUSH_TIMEOUT_SECS: u64 = 10;

/// Values the client checks during the handshake; defaults match the
/// Infinity build 3653293 start.ini and eveMachoNetVersion.machoVersion.
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub database_pool_size: usize,
    pub macho_version: i64,
    pub boot_version: f64,
    pub boot_build: i64,
    pub boot_codename: String,
    pub boot_region: String,
    /// Node ID the client sees as its proxy.
    pub node_id: i64,
    /// Address clients use to reach this host (chat, image server URLs).
    pub public_host: String,
    pub chat_bind: String,
    /// XMPP domain: users are <charID>@domain, rooms <name>@conference.domain.
    pub chat_domain: String,
    pub image_bind: String,
    pub image_port: u16,
    pub image_dir: String,
    /// Where missing NPC portraits, NPC corporation logos and type images are fetched
    /// from once (empty: never); see images.rs.
    pub image_upstream: String,
    pub gateway_bind: String,
    /// Content CDN (plain HTTP); the client always fetches it on port 80.
    pub cdn_bind: String,
    /// Pilots allowed in space per solar system before stargate jumps queue (0 = no limit).
    pub system_capacity: usize,
}

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Config {
    fn from_env() -> Result<Config> {
        Ok(Config {
            bind: env_or("BIND", "0.0.0.0:26000".to_string()),
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is not set")?,
            database_pool_size: env_or("DATABASE_POOL_SIZE", DATABASE_POOL_SIZE_DEFAULT),
            macho_version: env_or("MACHO_VERSION", 496),
            boot_version: env_or("BOOT_VERSION", 24.01),
            boot_build: env_or("BOOT_BUILD", 3653293),
            boot_codename: env_or("BOOT_CODENAME", "EVE-RELEASE".to_string()),
            boot_region: env_or("BOOT_REGION", "optic".to_string()),
            node_id: env_or("NODE_ID", 888444),
            public_host: env_or("PUBLIC_HOST", "127.0.0.1".to_string()),
            chat_bind: env_or("CHAT_BIND", "0.0.0.0:5222".to_string()),
            chat_domain: env_or("CHAT_DOMAIN", "infinity".to_string()),
            image_bind: env_or("IMAGE_BIND", "0.0.0.0:26001".to_string()),
            image_port: env_or("IMAGE_PORT", 26001),
            image_dir: env_or("IMAGE_DIR", "/data/images".to_string()),
            image_upstream: env_or("IMAGE_UPSTREAM", "https://images.evetech.net".to_string()),
            gateway_bind: env_or("GATEWAY_BIND", "0.0.0.0:26002".to_string()),
            cdn_bind: env_or("CDN_BIND", "0.0.0.0:80".to_string()),
            system_capacity: env_or("SYSTEM_CAPACITY", 0),
        })
    }

    pub fn cdn_url(&self) -> String {
        format!("http://{}", self.public_host)
    }

    /// machoNet global config handed to the client (handshake config_vals, GetGlobalConfig).
    /// banned_words_enabled: the chat window drops every line when the word filter is off
    /// (bannedwords_client.replace_chat_banned_words returns None); with it on, chat is
    /// checked against the retail regexes (retail::cached_call bannedwords.fetch_regexes).
    pub fn global_config(&self) -> marshal::Value {
        marshal::Value::dict([
            ("imageserverurl", marshal::Value::str(format!("http://{}:{}/", self.public_host, self.image_port))),
            ("banned_words_enabled", marshal::Value::str("1")),
        ])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,infinity_server=debug".into()))
        .init();
    let cfg = Arc::new(Config::from_env()?);
    let db = db::Db::connect(&cfg.database_url, cfg.database_pool_size).await?;
    services::recover_pending_reloads(&db).await.map_err(|error| match error {
        services::CallError::Internal(e) => e,
        services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
    })?;
    if let Err(services::CallError::Internal(e)) = services::unbury_sites(&db).await {
        tracing::warn!("moving sites out of planets failed: {e:#}");
    }
    let hub = chat::Hub::new(&cfg.chat_domain);
    let world = space::World::new(db.clone(), cfg.system_capacity);
    tokio::spawn(services::expire_market_orders(db.clone(), world.clone()));
    tokio::spawn(services::settle_auctions(db.clone(), world.clone()));
    tokio::spawn(services::run_billing(db.clone(), world.clone()));
    let gateway_task = tokio::spawn(log_exit("public gateway", gateway::serve(cfg.gateway_bind.clone(), gateway::Gateway::new(db.clone(), cfg.cdn_url()))));
    let chat_task = tokio::spawn(log_exit("chat", chat::serve(cfg.chat_bind.clone(), hub.clone())));
    let upstream = images::Upstream::new(&cfg.image_upstream);
    let image_task = tokio::spawn(log_exit("image server", images::serve("images", cfg.image_bind.clone(), cfg.image_dir.clone().into(), upstream.clone())));
    let cdn_task = tokio::spawn(log_exit("content cdn", images::serve("cdn", cfg.cdn_bind.clone(), cfg.image_dir.clone().into(), upstream)));
    let listener = TcpListener::bind(&cfg.bind).await.with_context(|| format!("binding {}", cfg.bind))?;
    info!("listening on {} (macho {}, build {}, {}@{})", cfg.bind, cfg.macho_version, cfg.boot_build, cfg.boot_codename, cfg.boot_region);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("registering SIGTERM handler")?;
    let shutdown = async {
        #[cfg(unix)]
        {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result.context("handling SIGINT"),
                result = sigterm.recv() => result.context("SIGTERM handler closed").map(|_| ()),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.context("handling SIGINT")
        }
    };
    tokio::pin!(shutdown);
    let next_client_id = Arc::new(AtomicI64::new(FIRST_CLIENT_ID));
    // The biased select polls and registers the signal handler before accepting clients.
    loop {
        let (stream, peer) = tokio::select! {
            biased;
            result = &mut shutdown => {
                result?;
                break;
            }
            result = listener.accept() => result?,
        };
        stream.set_nodelay(true).ok();
        let (cfg, db, ids, hub, world) = (cfg.clone(), db.clone(), next_client_id.clone(), hub.clone(), world.clone());
        tokio::spawn(async move {
            let client_id = ids.fetch_add(1, Ordering::Relaxed);
            let conn = conn::Conn::new(stream, peer);
            info!("connection from {peer} (clientID {client_id})");
            let (mut conn, login, mut registration) = match handshake::run(conn, &cfg, &db, client_id).await {
                Ok(Some(triple)) => triple,
                Ok(None) => {
                    info!("{peer} closed during handshake");
                    return;
                }
                Err(e) => {
                    error!("{peer} handshake failed: {e:#}");
                    return;
                }
            };
            if registration.is_cancelled() {
                if let Err(e) = conn.send(&conn::usurped_close()).await {
                    warn!("{peer} usurp close failed: {e:#}");
                }
                drop(conn);
                drop(registration);
                return;
            }
            db.add_online(ONLINE_JOIN_DELTA);
            if let Err(e) = client::Client::new(conn, cfg, db.clone(), hub, world, login).run(&mut registration).await {
                error!("{peer} session ended with error: {e:#}");
            }
            db.add_online(ONLINE_LEAVE_DELTA);
            drop(registration);
        });
    }
    drop(listener);
    gateway_task.abort();
    chat_task.abort();
    image_task.abort();
    cdn_task.abort();
    let _ = tokio::join!(gateway_task, chat_task, image_task, cdn_task);
    match tokio::time::timeout(
        std::time::Duration::from_secs(POSITION_FLUSH_TIMEOUT_SECS),
        world.flush_positions(),
    ).await {
        Ok(Ok(positions_written)) => info!(positions_written, "position snapshot shutdown flush"),
        Ok(Err(e)) => {
            warn!("position snapshot shutdown flush failed: {e:#}");
            info!(outcome = "failed", "position snapshot shutdown flush");
        }
        Err(_) => {
            warn!("position snapshot shutdown flush timed out");
            info!(outcome = "timed out", "position snapshot shutdown flush");
        }
    }
    Ok(())
}


async fn log_exit(what: &'static str, f: impl std::future::Future<Output = Result<()>>) {
    if let Err(e) = f.await {
        error!("{what} stopped: {e:#}");
    }
}
