//! Fetches and validates complete ESI corporation responses before any writes.
//! The client `lpStoreSvc.py` consumes the catalogue, not this operator module.
//! ESI supplies offers; SDE types constrain IDs. Pagination is not supported by
//! this ESI endpoint; HTTP, oversized response and payload failures abort the whole corporation.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) struct Offer {
    pub offer_id: i64,
    pub type_id: i32,
    pub quantity: i32,
    pub lp_cost: i64,
    pub isk_cost: i64,
    pub ak_cost: i64,
    pub required_items: Value,
}

fn integer(value: &Value, field: &str, minimum: i64) -> Result<i64> {
    let n = value.get(field).and_then(Value::as_i64).with_context(|| format!("{field} must be an integer"))?;
    if n < minimum { bail!("{field} must be at least {minimum}"); }
    Ok(n)
}

fn positive_i32(value: &Value, field: &str) -> Result<i32> {
    i32::try_from(integer(value, field, 1)?).with_context(|| format!("{field} exceeds integer range"))
}

fn type_id(value: &Value, types: &HashSet<i32>) -> Result<i32> {
    let id = positive_i32(value, "type_id")?;
    if !types.contains(&id) { bail!("type_id {id} is not in the operator SDE"); }
    Ok(id)
}

pub(crate) fn parse(body: &[u8], types: &HashSet<i32>) -> Result<Vec<Offer>> {
    let value: Value = serde_json::from_slice(body).context("invalid ESI JSON")?;
    let entries = value.as_array().context("ESI response must be an array")?;
    let mut ids = HashSet::with_capacity(entries.len());
    let mut offers = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let offer = (|| -> Result<Offer> {
            let offer_id = integer(entry, "offer_id", 1)?;
            if !ids.insert(offer_id) { bail!("duplicate offer_id {offer_id}"); }
            let mut items = Vec::new();
            for item in entry.get("required_items").and_then(Value::as_array).context("required_items must be an array")? {
                items.push(json!({"type_id": type_id(item, types)?, "quantity": positive_i32(item, "quantity")?}));
            }
            Ok(Offer {
                offer_id,
                type_id: type_id(entry, types)?,
                quantity: positive_i32(entry, "quantity")?,
                lp_cost: integer(entry, "lp_cost", 0)?,
                isk_cost: integer(entry, "isk_cost", 0)?,
                ak_cost: if entry.get("ak_cost").is_none() { 0 } else { integer(entry, "ak_cost", 0)? },
                required_items: Value::Array(items),
            })
        })().with_context(|| format!("offer at index {index}"))?;
        offers.push(offer);
    }
    offers.sort_unstable_by_key(|offer| offer.offer_id);
    Ok(offers)
}

pub(crate) async fn fetch(client: &reqwest::Client, base: &str, corporation_id: i64, types: &HashSet<i32>, max_response_bytes: usize) -> Result<Vec<Offer>> {
    if max_response_bytes == 0 { bail!("maximum ESI response size must be nonzero"); }
    let url = format!("{}/loyalty/stores/{corporation_id}/offers/", base.trim_end_matches('/'));
    let mut response = client.get(url).send().await.context("fetching ESI offers")?.error_for_status().context("ESI returned an error status")?;
    if response.content_length().is_some_and(|length| u128::from(length) > max_response_bytes as u128) {
        bail!("ESI response exceeds {max_response_bytes} bytes");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.context("reading ESI response")? {
        if body.len().checked_add(chunk.len()).is_none_or(|length| length > max_response_bytes) {
            bail!("ESI response exceeds {max_response_bytes} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    parse(&body, types)
}

#[cfg(test)]
mod tests;
