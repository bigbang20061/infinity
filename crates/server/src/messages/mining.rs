//! Player-visible refusals for the personal mining ledger. The ledger client is
//! `eve/client/script/ui/shared/ledger/ledgerSvc.py`; this module reads no data
//! and does not define corporate observer messages.

pub(crate) const INVALID_LEDGER_REQUEST: &str = "采矿记录请求包含无效参数。";
