//! Character and corporation wallet balances, credits and debits, and loyalty points.
//! Uses `game.characters`, `game.corp_wallets` and `game.lp_balances`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Corporation loyalty point wallets are unsupported by these queries; economy policy,
//! transactions, refusals, notifications and World are caller concerns.

pub(crate) mod characters;
pub(crate) mod corporations;
pub(crate) mod loyalty;

struct Balance(f64);

impl Balance {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self(row.get("balance"))
    }
}
