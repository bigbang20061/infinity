//! Character and corporation wallet balances, credits and debits, and loyalty points.
//! Uses `game.characters`, `game.corp_wallets` and `game.lp_balances`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Corporation loyalty point wallets are unsupported by these queries; economy policy,
//! transactions, refusals, notifications and World are caller concerns.
//! ISK storage is exact numeric; client balances are lossy doubles clamped by wallet type.
//! Corporation GM writes are unsupported; overflowing non-GM credits are database errors.
//! No decimal dependency is required for exact storage.

pub(crate) mod characters;
pub(crate) mod corporations;
pub(crate) mod loyalty;

pub(in crate::dao) fn client_isk(balance: f64, cap: f64) -> f64 {
    balance.min(cap)
}

struct Balance(f64);

impl Balance {
    fn from_row(row: &tokio_postgres::Row, cap: f64) -> Self {
        Self(client_isk(row.get("balance"), cap))
    }
}

#[cfg(test)]
mod tests;
