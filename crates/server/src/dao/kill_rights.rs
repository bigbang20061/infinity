//! Kill-right persistence for `eve/client/script/ui/shared/killRights.py` and
//! `eve/client/script/ui/shared/neocom/charsheet/killRightsPanel.py`.
//! Uses `game.kill_rights`; callers own transactions, timers and notifications.
//! GM operations and in-memory suspect flags are not handled here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct KillRight {
    pub kill_right_id: i64,
    pub from_id: i64,
    pub to_id: i64,
    pub expiry: i64,
    pub price: Option<f64>,
    pub restricted_to: Option<i64>,
    pub activated_until: Option<i64>,
    pub activated_by: Option<i64>,
    pub used_at: Option<i64>,
}
impl KillRight {
    fn from_row(r: &Row) -> Self {
        Self { kill_right_id: r.get("kill_right_id"), from_id: r.get("from_id"), to_id: r.get("to_id"), expiry: r.get("expiry"), price: r.get("price"), restricted_to: r.get("restricted_to"), activated_until: r.get("activated_until"), activated_by: r.get("activated_by"), used_at: r.get("used_at") }
    }
}
const COLUMNS: &str = "kill_right_id, from_id, to_id, expiry, price::float8 AS price, restricted_to, activated_until, activated_by, used_at";
/// Must run inside a transaction so expired cleanup and insertion are atomic.
pub(crate) async fn create(ex: &(impl Executor + ?Sized), from_id: i64, to_id: i64, now: i64, expiry: i64) -> Result<Option<KillRight>, db::Error> {
    ex.execute("DELETE FROM game.kill_rights WHERE from_id = $1 AND to_id = $2 AND used_at IS NULL AND expiry <= $3", &[&from_id, &to_id, &now]).await?;
    let sql = format!("INSERT INTO game.kill_rights (from_id, to_id, created_at, expiry) VALUES ($1, $2, $3, $4) ON CONFLICT (from_id, to_id) WHERE used_at IS NULL DO NOTHING RETURNING {COLUMNS}");
    Ok(ex.query_opt(&sql, &[&from_id, &to_id, &now, &expiry]).await?.as_ref().map(KillRight::from_row))
}
/// Includes expired and consumed rows so callers can distinguish refusals.
pub(crate) async fn get(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<KillRight>, db::Error> {
    let sql = format!("SELECT {COLUMNS} FROM game.kill_rights WHERE kill_right_id = $1");
    Ok(ex.query_opt(&sql, &[&id]).await?.as_ref().map(KillRight::from_row))
}
/// Active rights in deterministic order for startup restoration.
pub(crate) async fn active_activations(ex: &(impl Executor + ?Sized), now: i64) -> Result<Vec<KillRight>, db::Error> {
    let sql = format!("SELECT {COLUMNS} FROM game.kill_rights WHERE used_at IS NULL AND expiry > $1 AND activated_until > $1 ORDER BY kill_right_id");
    Ok(ex.query(&sql, &[&now]).await?.iter().map(KillRight::from_row).collect())
}
pub(crate) async fn mine(ex: &(impl Executor + ?Sized), char_id: i64, now: i64) -> Result<Vec<KillRight>, db::Error> {
    let sql = format!("SELECT {COLUMNS} FROM game.kill_rights WHERE (from_id = $1 OR to_id = $1) AND used_at IS NULL AND expiry > $2 ORDER BY kill_right_id");
    Ok(ex.query(&sql, &[&char_id, &now]).await?.iter().map(KillRight::from_row).collect())
}
pub(crate) async fn visible(ex: &(impl Executor + ?Sized), targets: &[i64], viewer_ids: &[i64], now: i64) -> Result<Vec<KillRight>, db::Error> {
    let sql = format!("SELECT {COLUMNS} FROM game.kill_rights WHERE to_id = ANY($1) AND used_at IS NULL AND expiry > $3 AND price IS NOT NULL AND (restricted_to IS NULL OR restricted_to = ANY($2)) ORDER BY kill_right_id");
    Ok(ex.query(&sql, &[&targets, &viewer_ids, &now]).await?.iter().map(KillRight::from_row).collect())
}
pub(crate) async fn sell(ex: &(impl Executor + ?Sized), id: i64, owner: i64, price: f64, restriction: Option<i64>, now: i64) -> Result<Option<KillRight>, db::Error> {
    let sql = format!("UPDATE game.kill_rights SET price = $3::float8::numeric, restricted_to = $4 WHERE kill_right_id = $1 AND from_id = $2 AND used_at IS NULL AND expiry > $5 AND (activated_until IS NULL OR activated_until <= $5) AND $3::float8 >= 0 RETURNING {COLUMNS}");
    Ok(ex.query_opt(&sql, &[&id, &owner, &price, &restriction, &now]).await?.as_ref().map(KillRight::from_row))
}
/// Returns the prior right; only a prior sale needs a cancellation notice.
pub(crate) async fn cancel(ex: &(impl Executor + ?Sized), id: i64, owner: i64, target: i64, now: i64) -> Result<Option<KillRight>, db::Error> {
    let sql = format!("WITH old AS (SELECT * FROM game.kill_rights WHERE kill_right_id = $1 AND from_id = $2 AND to_id = $3 AND used_at IS NULL AND expiry > $4 AND (activated_until IS NULL OR activated_until <= $4) FOR UPDATE), changed AS (UPDATE game.kill_rights k SET price = NULL, restricted_to = NULL FROM old WHERE k.kill_right_id = old.kill_right_id RETURNING k.kill_right_id) SELECT {COLUMNS} FROM old WHERE kill_right_id IN (SELECT kill_right_id FROM changed)");
    Ok(ex.query_opt(&sql, &[&id, &owner, &target, &now]).await?.as_ref().map(KillRight::from_row))
}
/// Guard the exact offer, visibility, buyer identity and state in the write itself.
pub(crate) async fn buy(ex: &(impl Executor + ?Sized), id: i64, price: f64, actor: i64, viewer_ids: &[i64], now: i64, until: i64, buyback: bool) -> Result<Option<KillRight>, db::Error> {
    let sql = format!("UPDATE game.kill_rights SET used_at = CASE WHEN $7 THEN $5::bigint ELSE NULL::bigint END, activated_until = CASE WHEN $7 THEN NULL::bigint ELSE $6::bigint END, activated_by = CASE WHEN $7 THEN NULL::bigint ELSE $3::bigint END, price = NULL, restricted_to = NULL WHERE kill_right_id = $1 AND price = $2::float8::numeric AND from_id <> $3 AND (restricted_to IS NULL OR restricted_to = ANY($4)) AND used_at IS NULL AND expiry > $5 AND (activated_until IS NULL OR activated_until <= $5) AND (($7 AND to_id = $3) OR (NOT $7 AND to_id <> $3)) RETURNING {COLUMNS}");
    Ok(ex.query_opt(&sql, &[&id, &price, &actor, &viewer_ids, &now, &until, &buyback]).await?.as_ref().map(KillRight::from_row))
}
/// A paid buyer can restore an existing activation without extending its deadline.
pub(crate) async fn activate(ex: &(impl Executor + ?Sized), id: i64, actor: i64, now: i64, until: i64) -> Result<Option<KillRight>, db::Error> {
    let sql = format!("UPDATE game.kill_rights SET activated_until = CASE WHEN activated_until > $3 THEN activated_until ELSE $4::bigint END, activated_by = CASE WHEN activated_until > $3 THEN activated_by ELSE $2::bigint END, price = NULL, restricted_to = NULL WHERE kill_right_id = $1 AND used_at IS NULL AND expiry > $3 AND ((activated_until > $3 AND activated_by = $2) OR ((activated_until IS NULL OR activated_until <= $3) AND from_id = $2)) RETURNING {COLUMNS}");
    Ok(ex.query_opt(&sql, &[&id, &actor, &now, &until]).await?.as_ref().map(KillRight::from_row))
}
/// Caller emits notices for every returned right within the killmail transaction.
pub(crate) async fn consume_active(ex: &(impl Executor + ?Sized), target: i64, now: i64) -> Result<Vec<KillRight>, db::Error> {
    let sql = format!("WITH active AS (SELECT kill_right_id FROM game.kill_rights WHERE to_id = $1 AND used_at IS NULL AND expiry > $2 AND activated_until > $2 ORDER BY kill_right_id FOR UPDATE) UPDATE game.kill_rights SET used_at = $2 WHERE kill_right_id IN (SELECT kill_right_id FROM active) AND used_at IS NULL AND expiry > $2 AND activated_until > $2 RETURNING {COLUMNS}");
    Ok(ex.query(&sql, &[&target, &now]).await?.iter().map(KillRight::from_row).collect())
}
pub(crate) async fn delete_expired(ex: &(impl Executor + ?Sized), id: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.kill_rights WHERE kill_right_id = $1 AND used_at IS NULL AND expiry <= $2", &[&id, &now]).await? == 1)
}
