//! Store data shared by New Eden Store and loyalty point store services.
//! Uses game retail prices and SDE catalogue data; live catalogue tables are unavailable.
pub(crate) mod catalogue;
pub(crate) mod purchases;
pub(crate) mod rewards;
