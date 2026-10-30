//! Character and corporation wallet persistence in `game.characters` and `game.corp_wallets`.
//! This DAO has no direct client mirror or wire contract; owning services supply those.
//! Economy policy, transactions, refusals, notifications and World are caller concerns.

pub(crate) mod characters;
pub(crate) mod corporations;
pub(crate) mod loyalty;

struct Balance(f64);

impl Balance {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self(row.get("balance"))
    }
}
