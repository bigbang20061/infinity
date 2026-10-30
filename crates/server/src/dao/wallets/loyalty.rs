//! Loyalty point persistence for LPSvc and LPStoreMgr.
//! Uses game.lp_balances; corporation wallets are not supported by these queries.

use crate::db::{executor::Executor, Error};

pub(crate) struct BalanceRow { pub corp_id: i64, pub amount: i64 }
impl BalanceRow {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { corp_id: row.get("corp_id"), amount: row.get("amount") } }
}
struct AmountRow { amount: i64 }
impl AmountRow {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { amount: row.get("amount") } }
}
pub(crate) async fn balance(ex: &(impl Executor + ?Sized), char_id: i64, corp: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT amount FROM game.lp_balances WHERE char_id = $1 AND corp_id = $2", &[&char_id, &corp]).await?.map(|r| AmountRow::from_row(&r).amount))
}
pub(crate) async fn add(ex: &(impl Executor + ?Sized), char_id: i64, corp: i64, amount: i64) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.lp_balances (char_id, corp_id, amount) VALUES ($1, $2, $3)
             ON CONFLICT (char_id, corp_id) DO UPDATE SET amount = game.lp_balances.amount + $3", &[&char_id, &corp, &amount]).await
}
pub(crate) async fn list(ex: &impl Executor, char_id: i64) -> Result<Vec<BalanceRow>, Error> {
    Ok(ex.query("SELECT corp_id, amount FROM game.lp_balances WHERE char_id = $1 AND amount > 0", &[&char_id]).await?.iter().map(BalanceRow::from_row).collect())
}
