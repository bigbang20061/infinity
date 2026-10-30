//! Internal guarded quantity writes for inventory operations. These update `game.items`,
//! the inventory table used by invCache; planning, transactions and notifications remain
//! the caller's responsibility. This module does not handle other item mutations.

use std::collections::BTreeMap;
use crate::db::executor::Executor;
use super::{CallError, db_err, notify_error};
use super::dogma::Item;

#[derive(Clone, Copy)]
pub(super) struct QuantityChange {
    pub expected_quantity: i32,
    pub quantity: Option<i32>,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub singleton: bool,
}

impl QuantityChange {
    pub(super) fn new(item: &Item, quantity: Option<i32>) -> Self {
        Self { expected_quantity: item.quantity, quantity, owner_id: item.owner_id, location_id: item.location_id, flag_id: item.flag_id, singleton: item.singleton }
    }
}

pub(super) async fn apply(db: &(impl Executor + ?Sized), changes: &BTreeMap<i64, QuantityChange>, refusal: &str) -> Result<(), CallError> {
    for (&id, change) in changes {
        let affected = match change.quantity {
            Some(quantity) => db.execute(
                "UPDATE game.items SET quantity = $7 WHERE item_id = $1 AND quantity = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton = $6",
                &[&id, &change.expected_quantity, &change.owner_id, &change.location_id, &change.flag_id, &change.singleton, &quantity],
            ).await,
            None => db.execute(
                "DELETE FROM game.items WHERE item_id = $1 AND quantity = $2 AND owner_id = $3 AND location_id = $4 AND flag_id = $5 AND singleton = $6",
                &[&id, &change.expected_quantity, &change.owner_id, &change.location_id, &change.flag_id, &change.singleton],
            ).await,
        }.map_err(db_err)?;
        if affected == 0 { return Err(notify_error(refusal)); }
    }
    Ok(())
}
