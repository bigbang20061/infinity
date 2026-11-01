//! Shareholder dividend receipts for the corporation UI in
//! `eve/client/script/ui/services/corporation`. Uses share holdings and dividend tables;
//! wallet changes, authorization, transactions and member dividends are not handled here.
use crate::db::{self, executor::Executor};

pub(crate) struct Allocation { pub(crate) holder_id: i64, pub(crate) amount: f64, pub(crate) corporation: bool }
impl Allocation {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { holder_id: r.get("holder_id"), amount: r.get("amount"), corporation: r.get("corporation") } }
}
struct Amount { value: f64 }
impl Amount { fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } } }
struct AllocationResult { safe: bool, holder_id: Option<i64> }
impl AllocationResult {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { safe: r.get("safe"), holder_id: r.get("holder_id") } }
}

pub(crate) async fn receipt_exists(ex: &(impl Executor + ?Sized), op: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.corp_dividends WHERE operation_id=$1", &[&op]).await?.is_some())
}
pub(crate) async fn claim(ex: &(impl Executor + ?Sized), op: i64, issuer: i64, key: i32, requested: f64, actor: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_dividends (operation_id, corporation_id, division, requested, actor_id, created) VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (operation_id) DO NOTHING", &[&op, &issuer, &key, &requested, &actor, &now]).await? != 0)
}
pub(crate) async fn allocate(ex: &(impl Executor + ?Sized), op: i64, issuer: i64, requested: f64) -> Result<Option<Vec<Allocation>>, db::Error> {
    // numeric(36,2) has 34 integer digits; wallet float8 values must round-trip exactly.
    let rows = ex.query("WITH eligible AS MATERIALIZED (SELECT s.shareholder_id AS holder_id, s.shares, (corp.corporation_id IS NOT NULL) AS corporation FROM game.corp_shares s LEFT JOIN game.characters ch ON ch.character_id=s.shareholder_id AND NOT ch.deleted LEFT JOIN game.corporations corp ON corp.corporation_id=s.shareholder_id AND NOT corp.deleted WHERE s.corporation_id=$2 AND s.shareholder_id<>$2 AND s.shares>0 AND (ch.character_id IS NOT NULL OR corp.corporation_id IS NOT NULL)), amounts AS MATERIALIZED (SELECT holder_id, shares, corporation, div(($3::float8)::numeric * shares * 100, sum(shares) OVER ()) / 100 AS amount FROM eligible), totals AS MATERIALIZED (SELECT COALESCE(sum(amount),0) AS total, COALESCE(bool_and(CASE WHEN amount >= 0 AND amount < 10000000000000000000000000000000000::numeric THEN amount::float8::numeric = amount ELSE false END),true) AS amounts_safe FROM amounts), safety AS MATERIALIZED (SELECT amounts_safe AND CASE WHEN total >= 0 AND total < 10000000000000000000000000000000000::numeric THEN total::float8::numeric = total ELSE false END AS safe FROM totals), inserted AS (INSERT INTO game.corp_dividend_recipients (operation_id, holder_id, shares, amount) SELECT $1, a.holder_id, a.shares, a.amount FROM amounts a CROSS JOIN safety WHERE safety.safe ORDER BY a.holder_id RETURNING holder_id, shares, amount) SELECT safety.safe, i.holder_id, i.amount::float8 AS amount, a.corporation FROM safety LEFT JOIN inserted i ON true LEFT JOIN amounts a ON a.holder_id=i.holder_id ORDER BY i.holder_id", &[&op, &issuer, &requested]).await?;
    if !AllocationResult::from_row(&rows[0]).safe { return Ok(None); }
    Ok(Some(rows.iter().filter(|r| AllocationResult::from_row(r).holder_id.is_some()).map(Allocation::from_row).collect()))
}
pub(crate) async fn distributed(ex: &(impl Executor + ?Sized), op: i64) -> Result<f64, db::Error> {
    Ok(Amount::from_row(&ex.query_one("UPDATE game.corp_dividends SET distributed = (SELECT COALESCE(sum(amount),0) FROM game.corp_dividend_recipients WHERE operation_id=$1) WHERE operation_id=$1 RETURNING distributed::float8 AS value", &[&op]).await?).value)
}
