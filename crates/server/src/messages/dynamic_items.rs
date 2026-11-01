//! Refusals for server-side mutaplasmid creation and information reads.
//! The client uses `eve/client/script/dogma/dynamicItemSvc.py`; no data is read here.
//! These messages do not implement unsupported mutation variants.
pub(crate) const INVALID_ARGUMENT: &str = "物品参数无效。";
pub(crate) const NOT_FOUND: &str = "未找到突变物品信息。";
pub(crate) const INVALID_ITEMS: &str = "物品不存在、数量不足或不属于你。";
pub(crate) const INCOMPATIBLE: &str = "突变质体与该物品不兼容。";
pub(crate) const DAMAGED: &str = "受损物品不能突变，请先修理。";
pub(crate) const INACCESSIBLE: &str = "物品不在可访问的本地库存中，或位于不允许的舱位。";
pub(crate) const DEPLOYED: &str = "已部署在太空中的物品不能突变。";
pub(crate) const OFFICE: &str = "无法使用此军团办公室或没有取用权限。";
pub(crate) const RESERVED: &str = "交易预留或正在制造任务中使用的物品不能突变。";
pub(crate) const CHANGED: &str = "物品已发生变化，请重试。";
pub(crate) const INVALID_DATA: &str = "无法生成此物品的属性。";
