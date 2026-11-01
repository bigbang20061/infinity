//! Player-visible corporation governance refusals for share transfers.
//! These texts accompany client corporation calls; they contain no database state.

pub(crate) const INVALID_SHARE_TRANSFER: &str = "无法转移这些股份。";
pub(crate) const SHARE_AUTHORITY_REQUIRED: &str = "你没有转移这些股份的权限。";
pub(crate) const SHARES_UNAVAILABLE: &str = "该军团的股份不可用。";
