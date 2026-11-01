//! Corporation persistence for registry, roles, enlistment, shares and dividends.
//! Services mirror `eve/client/script/ui/services/corporation` calls.
//! Sources include corporations, characters, share holdings and receipts.
//! Policy, transactions and notifications are not implemented here; votes are unsupported.

pub(crate) mod enlistment;
pub(crate) mod dividends;
pub(crate) mod shares;
pub(crate) mod lookup;
pub(crate) mod registry;
pub(crate) mod dispatch;
pub(crate) mod roles;
pub(crate) mod wars;
