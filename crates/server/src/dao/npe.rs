//! Persistence for AIR NPE character state in `game.air_npe_state`.
//! The client-facing protocol, defaults and notifications are owned by the service.

use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

pub(crate) struct NpeState {
    pub(crate) state: i16,
}

impl NpeState {
    fn from_row(row: &Row) -> Self {
        Self { state: row.get("state") }
    }
}

pub(crate) async fn state_of(ex: &(impl Executor + ?Sized), char_id: i64) -> Result<Option<NpeState>, Error> {
    Ok(ex.query_opt("SELECT state FROM game.air_npe_state WHERE character_id = $1", &[&char_id])
        .await?
        .as_ref()
        .map(NpeState::from_row))
}

pub(crate) async fn set_state(ex: &(impl Executor + ?Sized), char_id: i64, state: i16, changed_at: i64) -> Result<u64, Error> {
    ex.execute(
        "INSERT INTO game.air_npe_state (character_id, state, changed_at) VALUES ($1, $2, $3)
             ON CONFLICT (character_id) DO UPDATE SET state = EXCLUDED.state, changed_at = EXCLUDED.changed_at",
        &[&char_id, &state, &changed_at],
    ).await
}
