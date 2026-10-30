//! Character training attributes, mirroring characterskills/util.py.
//! Uses game.characters and sde.chr_bloodlines; missing values use a fallback.
//! Attribute modifiers are not modelled here.
use super::{db_err, CallError};
use crate::client::Client;
use crate::db::executor::Executor;
use std::collections::HashMap;

/// SDE dgmAttributeTypes: charisma, intelligence, memory, perception, willpower.
pub(super) const CHAR_ATTRIBUTES: [i64; 5] = [164, 165, 166, 167, 168];
/// Server fallback for missing SDE bloodline attributes.
const FALLBACK_BLOODLINE_ATTRIBUTE: i32 = 5;
/// Server character attribute calculation offset.
const CHARACTER_ATTRIBUTE_OFFSET: f64 = 17.0;

/// Character attribute values {attributeID: value}.
pub(crate) async fn character_attributes(c: &Client, char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    character_attributes_db(&c.db, char_id).await
}

pub(super) async fn character_attributes_db(db: &(impl Executor + ?Sized), char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    let r = crate::dao::characters::attributes::bloodline(db, char_id).await.map_err(db_err)?;
    let values = [r.charisma, r.intelligence, r.memory, r.perception, r.willpower];
    Ok(CHAR_ATTRIBUTES.iter().zip(values).map(|(a, value)| (*a, value.unwrap_or(FALLBACK_BLOODLINE_ATTRIBUTE) as f64 + CHARACTER_ATTRIBUTE_OFFSET)).collect())
}
