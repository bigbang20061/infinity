//! Player-visible corporation governance refusals for share transfers and dividends.
//! These texts accompany client corporation calls; they contain no database state.

pub(crate) const INVALID_SHARE_TRANSFER: &str = "无法转移这些股份。";
pub(crate) const SHARE_AUTHORITY_REQUIRED: &str = "你没有转移这些股份的权限。";
pub(crate) const SHARES_UNAVAILABLE: &str = "该军团的股份不可用。";
pub(crate) const INVALID_DIVIDEND: &str = "股息金额无效。";
pub(crate) const DIVIDEND_AUTHORITY_REQUIRED: &str = "只有军团 CEO 才能发放股息。";
pub(crate) const MEMBER_DIVIDEND_UNSUPPORTED: &str = "暂不支持向军团成员平均发放股息。";
pub(crate) const NO_DIVIDEND_RECIPIENTS: &str = "没有符合条件的股息收款人。";
