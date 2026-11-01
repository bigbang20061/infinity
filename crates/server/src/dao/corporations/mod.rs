//! Corporation persistence for registry, roles, enlistment and shares.
//! Services mirror `eve/client/script/ui/services/corporation` calls.
//! Sources include corporations, characters, share holdings and transfer receipts.
//! Policy, transactions and notifications are not implemented here; dividends and votes are unsupported.

pub(crate) mod enlistment;
pub(crate) mod shares;
pub(crate) mod lookup;
pub(crate) mod registry;
pub(crate) mod dispatch;
pub(crate) mod roles;
pub(crate) mod wars;
