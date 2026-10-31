//! Durable LP store offers from `game.lp_offers` for the client LP store.
//! The live catalogue is not shipped with the client; operators supply its contents.
//! Settlement, fallback offers and wire encoding are caller concerns.

use crate::db::{self, executor::Executor};
use serde::Deserialize;
use tokio_postgres::{Row, types::{FromSql, Type}};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct RequiredItem {
    pub type_id: i32,
    pub quantity: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    pub corporation_id: i64,
    pub offer_id: i64,
    pub type_id: i32,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: i64,
    pub ak_cost: i64,
    pub required_items: Vec<RequiredItem>,
}

// Decoding through FromSql converts malformed stored JSON into db::Error rather than
// silently omitting an offer or requirement. JSON arrays retain order and duplicates.
struct Requirements(Vec<RequiredItem>);
impl<'a> FromSql<'a> for Requirements {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let text = <&str as FromSql>::from_sql(ty, raw)?;
        let items: Vec<RequiredItem> = serde_json::from_str(text)?;
        if items.iter().any(|item| item.type_id <= 0 || item.quantity <= 0) {
            return Err("LP offer requirement type and quantity must be positive".into());
        }
        Ok(Self(items))
    }
    fn accepts(ty: &Type) -> bool { <&str as FromSql>::accepts(ty) }
}

impl Offer {
    fn from_row(row: &Row) -> Result<Self, db::Error> {
        Ok(Self {
            corporation_id: row.try_get("corporation_id")?,
            offer_id: row.try_get("offer_id")?,
            type_id: row.try_get("type_id")?,
            quantity: row.try_get("quantity")?,
            lp_cost: row.try_get("lp_cost")?,
            isk_cost: row.try_get("isk_cost")?,
            ak_cost: row.try_get("ak_cost")?,
            required_items: row.try_get::<_, Requirements>("required_items")?.0,
        })
    }
}

pub(crate) async fn list(ex: &(impl Executor + ?Sized), corporation_id: i64) -> Result<Vec<Offer>, db::Error> {
    ex.query("SELECT corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items::text AS required_items FROM game.lp_offers WHERE corporation_id = $1 ORDER BY offer_id", &[&corporation_id]).await?.iter().map(Offer::from_row).collect()
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), corporation_id: i64, offer_id: i64) -> Result<Option<Offer>, db::Error> {
    ex.query_opt("SELECT corporation_id, offer_id, type_id, quantity, lp_cost, isk_cost, ak_cost, required_items::text AS required_items FROM game.lp_offers WHERE corporation_id = $1 AND offer_id = $2", &[&corporation_id, &offer_id]).await?.as_ref().map(Offer::from_row).transpose()
}

pub(crate) async fn is_empty(ex: &(impl Executor + ?Sized)) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.lp_offers LIMIT 1", &[]).await?.is_none())
}

#[cfg(test)]
mod tests;
