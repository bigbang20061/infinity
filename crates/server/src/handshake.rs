//! Server side of the GPS login handshake, mirroring
//! `GPSTransport.HandleClientAuthentication` in carbon/common/script/net/GPS.py,
//! with the client's `evecrypto.placebo` crypto pack (start.ini: cryptoPack = Placebo).

use crate::conn::Conn;
use crate::db::Db;
use crate::Config;
use anyhow::{anyhow, bail, Context, Result};
use marshal::Value;
use tracing::{info, warn};

const HANDSHAKE_SECRET_KEY: i64 = 170472;
const PASSWORD_VERSION: i64 = 2; // client sends a hash, never the plaintext

/// What a finished handshake hands to the session layer.
pub struct Login {
    pub user_id: i64,
    pub user_name: String,
    pub role: i64,
    pub client_id: i64,
    pub session_id: i64,
    pub language_id: String,
}

/// `evecrypto.placebo.crypto_hash(*args)`: `str(binascii.crc_hqx(blue.marshal.Save(args), 0))`.
pub fn placebo_hash(args: Vec<Value>) -> Value {
    Value::str(crc_hqx(&marshal::dumps(&Value::Tuple(args)), 0).to_string())
}

/// CRC-CCITT as in `binascii.crc_hqx` (poly 0x1021, MSB first).
fn crc_hqx(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn handshake_tuple(cfg: &Config, user_count: i64) -> Value {
    Value::tuple([
        Value::Int(HANDSHAKE_SECRET_KEY),
        Value::Int(cfg.macho_version),
        Value::Int(user_count),
        Value::Float(cfg.boot_version),
        Value::Int(cfg.boot_build),
        Value::str(format!("{}@{}", cfg.boot_codename, cfg.boot_region)),
        Value::None, // update info (const.responseUnknown on a real proxy)
    ])
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_text()).map(|s| s.into_owned())
}

/// Run the handshake until a login completes. Status probes (`(None, 'QC')`)
/// are answered and the handshake restarts on the same socket, exactly like
/// the proxy's accept loop, because the client reuses the probed transport.
pub async fn run(conn: &mut Conn, cfg: &Config, db: &Db, client_id: i64) -> Result<Option<Login>> {
    loop {
        conn.send(&handshake_tuple(cfg, db.online_count())).await?;

        // Step 1: client compatibility tuple.
        let Some(step1) = conn.recv().await? else { return Ok(None) };
        let s1 = step1.items().ok_or_else(|| anyhow!("step1 is not a tuple: {step1:?}"))?;
        if s1.len() != 6 || s1[0].as_int() != Some(HANDSHAKE_SECRET_KEY) {
            bail!("bad compatibility handshake: {step1:?}");
        }
        if s1[1].as_int() != Some(cfg.macho_version) || s1[4].as_int() != Some(cfg.boot_build) {
            warn!("client reports macho {:?} build {:?}; server is {} / {}", s1[1], s1[4], cfg.macho_version, cfg.boot_build);
        }

        // Step 2: optional VIP key, then queue check or crypto context.
        let Some(mut step2) = conn.recv().await? else { return Ok(None) };
        if is_marker(&step2, "VK") {
            let Some(next) = conn.recv().await? else { return Ok(None) };
            step2 = next;
        }
        if is_marker(&step2, "QC") {
            info!("status probe from {}", conn.peer);
            conn.send(&Value::Int(1)).await?; // logon queue position
            continue;
        }
        // (publicKeyVersion, cryptoRequest) -> placebo context needs nothing.
        conn.send(&Value::str("OK CC")).await?;

        // Step 3: (clientChallenge, request dict).
        let Some(step3) = conn.recv().await? else { return Ok(None) };
        let s3 = step3.items().ok_or_else(|| anyhow!("step3 is not a tuple"))?;
        let (client_challenge, request) = match s3 {
            [c, r @ Value::Dict(_)] => (c.clone(), r.clone()),
            _ => bail!("bad secure handshake: {step3:?}"),
        };
        conn.send(&Value::Int(PASSWORD_VERSION)).await?;

        let user_name = text(request.get("user_name")).unwrap_or_default();
        let password_hash = request.get("user_password_hash").and_then(|v| v.as_bytes()).map(|b| b.to_vec());
        let language_id = text(request.get("user_languageid")).unwrap_or_else(|| "EN".into());

        let response = Value::dict([
            ("challenge_responsehash", placebo_hash(vec![client_challenge])),
            ("macho_version", Value::Int(cfg.macho_version)),
            ("boot_version", Value::Float(cfg.boot_version)),
            ("boot_build", Value::Int(cfg.boot_build)),
            ("boot_codename", Value::str(&cfg.boot_codename)),
            ("boot_region", Value::str(&cfg.boot_region)),
            ("cluster_usercount", Value::Int(db.online_count())),
            ("proxy_nodeid", Value::Int(cfg.node_id)),
            ("user_logonqueueposition", Value::Int(1)),
            ("config_vals", cfg.global_config()),
        ]);
        // Placebo Sign(x) == (x, 0). The "handshake function" is CPython-marshalled
        // and eval()'d by the client; the string "{}" evaluates to an empty dict.
        let handshake_func = Value::str(b"s\x02\x00\x00\x00{}");
        let server_challenge = Value::str(vec![0u8; 64]);
        conn.send(&Value::tuple([server_challenge, Value::tuple([handshake_func, Value::Int(0)]), Value::Dict(vec![]), response]))
            .await?;

        if user_name.is_empty() || password_hash.is_none() {
            bail!("login without credentials");
        }

        // Step 4: (hash(serverChallenge), funcOutput, funcResult).
        let Some(step4) = conn.recv().await? else { return Ok(None) };
        if let Some(out) = step4.items().and_then(|s| s.get(1)).and_then(|v| v.as_text()) {
            if !out.is_empty() {
                warn!("handshake function output: {out}");
            }
        }

        let account = db.login(&user_name, password_hash.as_deref().unwrap()).await.context("account lookup")?;
        let Some(account) = account else {
            warn!("login rejected for {user_name:?}: bad password");
            return Ok(None);
        };
        let session_id = rand_i64();
        let session_init = Value::dict([
            ("userid", Value::Int(account.user_id)),
            ("userType", Value::Int(account.user_type)),
            ("role", Value::Int(account.role)),
            ("languageID", Value::str(&language_id)),
            ("address", Value::str(conn.peer.ip().to_string())),
            ("inDetention", Value::None),
            ("maxSessionTime", Value::None),
            ("countryCode", Value::None),
        ]);
        let ack = Value::dict([
            ("sessionID", Value::Int(session_id)),
            ("user_clientid", Value::Int(client_id)),
            ("session_init", session_init),
            ("access_token", Value::None),
            ("computer_hash", Value::None),
            ("live_updates", Value::list([])),
            ("client_hash", Value::None),
        ]);
        conn.send(&ack).await?;
        info!("user {user_name:?} (id {}) logged in from {}", account.user_id, conn.peer);
        return Ok(Some(Login { user_id: account.user_id, user_name, role: account.role, client_id, session_id, language_id }));
    }
}

fn is_marker(v: &Value, m: &str) -> bool {
    matches!(v.items(), Some([Value::None, tag, ..]) if tag.as_text().is_some_and(|t| t == m))
}

pub fn rand_i64() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    (h.finish() >> 2) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_hqx_matches_python() {
        // binascii.crc_hqx(b'123456789', 0) == 0x31C3
        assert_eq!(crc_hqx(b"123456789", 0), 0x31C3);
        assert_eq!(crc_hqx(b"", 0), 0);
    }
}
