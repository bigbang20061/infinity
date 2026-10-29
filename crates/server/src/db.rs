//! PostgreSQL access. One pipelined connection shared by all tasks.
//! The shared account PLEX query returns whole PLEX; wire-unit conversion is the caller's policy.

use crate::consts::{NPC_BID_RATIO, ROLE_PLAYER_LOGIN};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio_postgres::{Client, NoTls};
use tracing::{error, info};

const GAME_SCHEMA: &str = include_str!("../sql/game_schema.sql");
const MARKET_SEED: &str = include_str!("../sql/market_seed.sql");

/// Existing server policy for the first database reconnect delay.
const RECONNECT_INITIAL_DELAY_SECONDS: u64 = 1;
/// Existing server policy for the largest database reconnect retry delay.
const RECONNECT_MAX_RETRY_DELAY_SECONDS: u64 = 16;
/// Existing server policy for exponential database reconnect backoff.
const RECONNECT_BACKOFF_FACTOR: u64 = 2;

#[derive(Clone)]
pub struct Db {
    pub client: Arc<Client>,
    online: Arc<AtomicI64>,
}

pub struct Account {
    pub user_id: i64,
    pub role: i64,
    pub user_type: i64,
}

impl Db {
    /// Account vault balance in whole PLEX, shared by the store and public gateway.
    pub async fn plex_balance(client: &Client, user_id: i64) -> Result<i64, tokio_postgres::Error> {
        Ok(client.query_one("SELECT plex_balance FROM game.accounts WHERE user_id = $1", &[&user_id]).await?.get(0))
    }

    pub async fn connect(url: &str) -> Result<Db> {
        let mut delay = RECONNECT_INITIAL_DELAY_SECONDS;
        let (client, conn) = loop {
            match tokio_postgres::connect(url, NoTls).await {
                Ok(c) => break c,
                Err(e) if delay <= RECONNECT_MAX_RETRY_DELAY_SECONDS => {
                    info!("database not ready ({e}); retrying in {delay}s");
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    delay *= RECONNECT_BACKOFF_FACTOR;
                }
                Err(e) => return Err(e).context("connecting to PostgreSQL"),
            }
        };
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("database connection lost: {e}");
            }
        });
        client.batch_execute(GAME_SCHEMA).await.context("applying game schema")?;
        let types: i64 = client.query_one("SELECT count(*) FROM sde.inv_types", &[]).await.context("SDE not imported? run sde-import")?.get(0);
        load_retail_prices(&client).await.context("loading retail prices")?;
        let seeded: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL)", &[]).await?.get(0);
        if !seeded {
            let n = client.execute(MARKET_SEED, &[]).await.context("seeding the market")?;
            info!("seeded {n} NPC market orders");
        }
        // NPC demand: a region-wide buy order beside every NPC sell order, so loot and ore
        // always sell somewhere.
        let bids_seeded: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM game.market_orders WHERE char_id IS NULL AND bid)", &[]).await?.get(0);
        if !bids_seeded {
            let n = client
                .execute(
                    "INSERT INTO game.market_orders (type_id, station_id, solar_system_id, constellation_id, region_id,
                                                     price, vol_entered, vol_remaining, bid, range, duration)
                     SELECT type_id, station_id, solar_system_id, constellation_id, region_id,
                            GREATEST(round((price * $1::float8)::numeric, 2), 0.01)::float8, 1000000, 1000000, true, 32767, 365
                     FROM game.market_orders WHERE char_id IS NULL AND NOT bid",
                    &[&NPC_BID_RATIO],
                )
                .await
                .context("seeding NPC buy orders")?;
            info!("seeded {n} NPC buy orders");
        }
        // NPC orders follow the retail price list (seeded before it was available, or updated since).
        let n = client
            .execute(
                "UPDATE game.market_orders o
                 SET price = GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)::float8
                 FROM game.retail_prices r
                 WHERE o.char_id IS NULL AND o.type_id = r.type_id AND r.average > 0
                   AND o.price <> GREATEST(round((r.average * CASE WHEN o.bid THEN $1::float8 ELSE 1 END)::numeric, 2), 0.01)::float8",
                &[&NPC_BID_RATIO],
            )
            .await?;
        if n > 0 {
            info!("repriced {n} NPC market orders from retail prices");
        }
        info!("database ready ({types} types in SDE)");
        Ok(Db { client: Arc::new(client), online: Arc::new(AtomicI64::new(0)) })
    }

    pub fn online_count(&self) -> i64 {
        self.online.load(Ordering::Relaxed)
    }
    pub fn add_online(&self, delta: i64) {
        self.online.fetch_add(delta, Ordering::Relaxed);
    }

    /// Verify credentials. Unknown user names are registered on first login.
    pub async fn login(&self, user_name: &str, password_hash: &[u8]) -> Result<Option<Account>> {
        let row = self
            .client
            .query_opt("SELECT user_id, password_hash, role, user_type FROM game.accounts WHERE lower(user_name) = lower($1)", &[&user_name])
            .await?;
        let (user_id, role, user_type) = match row {
            Some(r) => {
                let stored: Vec<u8> = r.get(1);
                if stored != password_hash {
                    return Ok(None);
                }
                (r.get::<_, i64>(0), r.get::<_, i64>(2), r.get::<_, i32>(3))
            }
            None => {
                let r = self
                    .client
                    .query_one(
                        "INSERT INTO game.accounts (user_name, password_hash, role) VALUES ($1, $2, $3) RETURNING user_id, user_type",
                        &[&user_name, &password_hash, &ROLE_PLAYER_LOGIN],
                    )
                    .await?;
                info!("registered new account {user_name:?}");
                (r.get(0), ROLE_PLAYER_LOGIN, r.get::<_, i32>(1))
            }
        };
        self.client.execute("UPDATE game.accounts SET last_login = now() WHERE user_id = $1", &[&user_id]).await?;
        Ok(Some(Account { user_id, role, user_type: user_type as i64 }))
    }
}

/// Replace `game.retail_prices` with the captured retail price list.
async fn load_retail_prices(client: &Client) -> Result<()> {
    let prices = crate::retail::average_prices();
    let (ids, (avg, adj)): (Vec<i32>, (Vec<f64>, Vec<f64>)) = prices.into_iter().map(|(t, a, j)| (t, (a, j))).unzip();
    client
        .execute(
            "INSERT INTO game.retail_prices (type_id, average, adjusted)
             SELECT * FROM unnest($1::int[], $2::float8[], $3::float8[])
             ON CONFLICT (type_id) DO UPDATE SET average = EXCLUDED.average, adjusted = EXCLUDED.adjusted",
            &[&ids, &avg, &adj],
        )
        .await?;
    info!("{} retail market prices loaded", ids.len());
    Ok(())
}
