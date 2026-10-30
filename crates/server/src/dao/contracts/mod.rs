//! Contract lifecycle, list, report, item, bid and status persistence for contractProxy.
//! The owning service supports `eve/client/script/ui/shared/neocom/contracts`.
//! Sources include game contracts, contract items, bids and items; static enrichment belongs to services.
//! Corporation contracts are unsupported; policy, transactions and notifications remain with callers.

pub(crate) mod lifecycle;
pub(crate) mod query;
pub(crate) mod reports;
pub(crate) mod shared;

pub(crate) struct Contract {
    pub(crate) id: i64,
    pub(crate) kind: i32,
    pub(crate) issuer: i64,
    pub(crate) issuer_corp: i64,
    pub(crate) assignee: i64,
    pub(crate) acceptor: i64,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) price: f64,
    pub(crate) reward: f64,
    pub(crate) collateral: f64,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) volume: f64,
    pub(crate) num_days: i32,
    pub(crate) status: i32,
    pub(crate) issued: i64,
    pub(crate) expires: i64,
    pub(crate) accepted: i64,
    pub(crate) completed: i64,
    pub(crate) package: Option<i64>,
}

pub(super) const CONTRACT_SELECT: &str = "SELECT c.contract_id, c.type, c.issuer_id, c.issuer_corp_id, c.assignee_id, c.acceptor_id,
        c.start_station_id, c.end_station_id,
        c.price, c.reward, c.collateral, c.title, c.description, c.volume, c.num_days, c.status,
        c.date_issued, c.date_expired, c.date_accepted, c.date_completed, c.package_id
    FROM game.contracts c";

impl Contract {
    pub(super) fn from_row(r: &tokio_postgres::Row) -> Self {
        Self {
            id: r.get("contract_id"), kind: r.get("type"), issuer: r.get("issuer_id"),
            issuer_corp: r.get("issuer_corp_id"), assignee: r.get("assignee_id"), acceptor: r.get("acceptor_id"),
            start: r.get("start_station_id"), end: r.get("end_station_id"),
            price: r.get("price"), reward: r.get("reward"), collateral: r.get("collateral"), title: r.get("title"),
            description: r.get("description"), volume: r.get("volume"), num_days: r.get("num_days"), status: r.get("status"),
            issued: r.get("date_issued"), expires: r.get("date_expired"), accepted: r.get("date_accepted"),
            completed: r.get("date_completed"), package: r.get("package_id"),
        }
    }
}
