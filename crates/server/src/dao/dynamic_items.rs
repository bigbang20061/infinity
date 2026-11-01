//! Mutation provenance in `game.dynamic_items` and typed factors in
//! `game.dynamic_item_modifiers`; effective values belong to `game.item_attributes`.
//! The client consumes dynamic item dogma through
//! `eve/common/script/dogma/baseDogmaLocation.py`; this DAO has no wire contract.
//! Mutation rules, transactions and notifications are not supported here.
use std::collections::BTreeMap;
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) struct MutationMetadata {
    pub(crate) item_id: i64,
    pub(crate) source_item_id: i64,
    pub(crate) mutator_item_id: i64,
    pub(crate) source_type_id: i32,
    pub(crate) mutator_type_id: i32,
    pub(crate) result_type_id: i32,
    pub(crate) creator_id: i64,
    pub(crate) created_at: i64,
    pub(crate) modifiers: BTreeMap<i32, f64>,
}

impl MutationMetadata {
    fn from_row(row: &Row) -> Self {
        let attribute_ids: Vec<i32> = row.get("attribute_ids");
        let factors: Vec<f64> = row.get("factors");
        Self {
            item_id: row.get("item_id"),
            source_item_id: row.get("source_item_id"),
            mutator_item_id: row.get("mutator_item_id"),
            source_type_id: row.get("source_type_id"),
            mutator_type_id: row.get("mutator_type_id"),
            result_type_id: row.get("result_type_id"),
            creator_id: row.get("creator_id"),
            created_at: row.get("created_at"),
            modifiers: attribute_ids.into_iter().zip(factors).collect(),
        }
    }
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), item: i64) -> Result<Option<MutationMetadata>, db::Error> {
    Ok(ex.query_opt(
        "SELECT d.item_id, d.source_item_id, d.mutator_item_id, d.source_type_id, d.mutator_type_id, d.result_type_id, d.creator_id, d.created_at, ARRAY(SELECT m.attribute_id FROM game.dynamic_item_modifiers m WHERE m.item_id = d.item_id ORDER BY m.attribute_id) AS attribute_ids, ARRAY(SELECT m.factor FROM game.dynamic_item_modifiers m WHERE m.item_id = d.item_id ORDER BY m.attribute_id) AS factors FROM game.dynamic_items d WHERE d.item_id = $1",
        &[&item],
    ).await?.as_ref().map(MutationMetadata::from_row))
}

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), metadata: &MutationMetadata) -> Result<(), db::Error> {
    ex.execute(
        "INSERT INTO game.dynamic_items (item_id, source_item_id, mutator_item_id, source_type_id, mutator_type_id, result_type_id, creator_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&metadata.item_id, &metadata.source_item_id, &metadata.mutator_item_id,
          &metadata.source_type_id, &metadata.mutator_type_id, &metadata.result_type_id,
          &metadata.creator_id, &metadata.created_at],
    ).await?;
    let (attribute_ids, factors): (Vec<i32>, Vec<f64>) = metadata.modifiers.iter().map(|(&id, &factor)| (id, factor)).unzip();
    ex.execute(
        "INSERT INTO game.dynamic_item_modifiers (item_id, attribute_id, factor) SELECT $1, attribute_id, factor FROM unnest($2::integer[], $3::double precision[]) AS modifiers(attribute_id, factor) ORDER BY attribute_id",
        &[&metadata.item_id, &attribute_ids, &factors],
    ).await?;
    Ok(())
}
