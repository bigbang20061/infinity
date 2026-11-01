//! Player-visible corporation governance refusals for share transfers, dividends and votes.
//! These texts accompany client corporation and voting calls; they contain no database state.

pub(crate) const INVALID_SHARE_TRANSFER: &str = "无法转移这些股份。";
pub(crate) const SHARE_AUTHORITY_REQUIRED: &str = "你没有转移这些股份的权限。";
pub(crate) const SHARES_UNAVAILABLE: &str = "该军团的股份不可用。";
pub(crate) const INVALID_DIVIDEND: &str = "股息金额无效。";
pub(crate) const DIVIDEND_AUTHORITY_REQUIRED: &str = "只有军团 CEO 才能发放股息。";
pub(crate) const MEMBER_DIVIDEND_UNSUPPORTED: &str = "暂不支持向军团成员平均发放股息。";
pub(crate) const NO_DIVIDEND_RECIPIENTS: &str = "没有符合条件的股息收款人。";
pub(crate) const VOTE_UNSUPPORTED: &str = "暂不支持这种投票。";
pub(crate) const VOTE_INVALID: &str = "投票内容无效。";
pub(crate) const VOTE_AUTHORITY_REQUIRED: &str = "你没有创建投票的权限。";
pub(crate) const VOTE_NOT_VISIBLE: &str = "你无权查看该投票。";
pub(crate) const VOTE_NOT_ELIGIBLE: &str = "你没有该投票的表决权。";
pub(crate) const VOTE_CLOSED: &str = "该投票已经结束。";
pub(crate) const VOTE_ALREADY_CAST: &str = "你已经投过票，不能更改选项。";
