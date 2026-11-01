//! Contract persistence for contractProxy (eve/client/script/ui/shared/neocom/contracts).
//! Reads game contract escrow and item counts; callers select the financial owner.

use crate::db::{executor::Executor, Error};

pub(crate) struct Escrow {
    pub isk: f64,
    pub items: f64,
}
impl Escrow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { isk: row.get("isk_escrow"), items: row.get("items_escrow") }
    }
}

pub(crate) async fn escrow(ex: &(impl Executor + ?Sized), me: i64, auction: i32, outstanding: i32, in_progress: i32) -> Result<Escrow, Error> {
    let row = ex.query_one(
        "SELECT (SELECT COALESCE(sum(reward), 0) FROM game.contracts WHERE (CASE WHEN for_corp THEN issuer_corp_id ELSE issuer_id END) = $1 AND type <> $2 AND status IN ($3, $4))
                  + (SELECT COALESCE(sum(b.amount), 0) FROM game.contract_bids b JOIN game.contracts k USING (contract_id)
                     WHERE b.bidder_id = $1 AND k.status = $3
                       AND b.amount = (SELECT max(amount) FROM game.contract_bids WHERE contract_id = b.contract_id)) AS isk_escrow,
                    (SELECT COALESCE(sum(ci.quantity * COALESCE(r.average, 0)), 0) FROM game.contract_items ci
                     JOIN game.contracts k USING (contract_id) LEFT JOIN game.retail_prices r USING (type_id)
                     WHERE (CASE WHEN k.for_corp THEN k.issuer_corp_id ELSE k.issuer_id END) = $1 AND ci.in_crate AND k.status IN ($3, $4)) AS items_escrow",
        &[&me, &auction, &outstanding, &in_progress],
    ).await?;
    Ok(Escrow::from_row(&row))
}

pub(crate) struct ContainerCount {
    pub location_id: i64,
    pub count: i64,
}
impl ContainerCount {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { location_id: row.get("location_id"), count: row.get("count") }
    }
}

pub(crate) async fn container_counts(ex: &(impl Executor + ?Sized), ids: &[i64], owner: i64) -> Result<Vec<ContainerCount>, Error> {
    Ok(ex.query("SELECT location_id, count(*) FROM game.items WHERE location_id = ANY($1) AND owner_id = $2 GROUP BY 1", &[&ids, &owner]).await?.iter().map(ContainerCount::from_row).collect())
}
