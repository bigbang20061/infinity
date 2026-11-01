//! Player-visible refusals for the personal mining ledger and in-space compression.
//! The clients are `eve/client/script/ui/shared/ledger/ledgerSvc.py` and
//! `itemcompression/client/itemCompression.py`; this module reads no data.

pub(crate) const INVALID_LEDGER_REQUEST: &str = "采矿记录请求包含无效参数。";
pub(crate) const COMPRESSION_ARGUMENT: &str = "压缩请求包含无效参数。";
pub(crate) const COMPRESSION_CONTEXT: &str = "只能在太空中使用当前飞船进行压缩。";
pub(crate) const COMPRESSION_ITEM: &str = "无法压缩该物品或其所在货舱。";
pub(crate) const COMPRESSION_TYPE: &str = "该物品无法压缩。";
pub(crate) const COMPRESSION_RESERVED: &str = "交易中预留的物品无法压缩。";
pub(crate) const COMPRESSION_FACILITY: &str = "压缩设施不可用、不受支持或超出范围。";
pub(crate) const COMPRESSION_CHANGED: &str = "物品已发生变化，请重试。";
pub(crate) const COMPRESSION_OVERFLOW: &str = "压缩后的堆叠数量超出上限。";
pub(crate) const COMPRESSION_AUTO: &str = "自动压缩尚不受支持。";
