//! Loyalty persistence for the client wallet and LP store services
//! (`eve/client/script/ui/shared/neocom/wallet/loyaltyPointsWalletSvc.py` and
//! `eve/client/script/ui/station/loyaltyPointStore/lpStoreSvc.py`).
//! `game.lp_balances` is keyed by character or corporation owner and issuer.
//! Offer catalogues, Analysis Kredits, authorization and notifications are not managed here.
use crate::db::{executor::Executor, Error};

pub(crate) struct BalanceRow { pub corp_id: i64, pub amount: i64 }
impl BalanceRow {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { corp_id: row.get("corp_id"), amount: row.get("amount") } }
}
struct AmountRow { amount: i64 }
impl AmountRow {
    fn from_row(row: &tokio_postgres::Row) -> Self { Self { amount: row.get("amount") } }
}
pub(crate) async fn balance(ex: &(impl Executor + ?Sized), owner: i64, corp: i64) -> Result<Option<i64>, Error> {
    Ok(ex.query_opt("SELECT amount FROM game.lp_balances WHERE char_id = $1 AND corp_id = $2", &[&owner, &corp]).await?.map(|r| AmountRow::from_row(&r).amount))
}
/// Atomically adjust a character or corporation owner's balance, refusing underflow or overflow; return its new value.
pub(crate) async fn add(ex: &(impl Executor + ?Sized), owner: i64, corp: i64, amount: i64) -> Result<Option<i64>, Error> {
    let max = i64::MAX;
    let row = ex.query_opt(
        "WITH debited AS (
             UPDATE game.lp_balances SET amount = amount + $3::bigint
             WHERE char_id = $1 AND corp_id = $2 AND $3::bigint < 0 AND amount::numeric + $3::bigint >= 0
             RETURNING amount
         ), credited AS (
             INSERT INTO game.lp_balances (char_id, corp_id, amount)
             SELECT $1, $2, $3::bigint WHERE $3::bigint > 0
             ON CONFLICT (char_id, corp_id) DO UPDATE SET amount = game.lp_balances.amount + $3::bigint
             WHERE game.lp_balances.amount::numeric + $3::bigint >= 0
               AND game.lp_balances.amount::numeric + $3::bigint <= $4::bigint
             RETURNING amount
         )
         SELECT amount FROM debited UNION ALL SELECT amount FROM credited",
        &[&owner, &corp, &amount, &max],
    ).await?;
    Ok(row.as_ref().map(|r| AmountRow::from_row(r).amount))
}
pub(crate) async fn list(ex: &(impl Executor + ?Sized), owner: i64) -> Result<Vec<BalanceRow>, Error> {
    Ok(ex.query("SELECT corp_id, amount FROM game.lp_balances WHERE char_id = $1 AND amount > 0", &[&owner]).await?.iter().map(BalanceRow::from_row).collect())
}

#[cfg(test)]
mod tests;
