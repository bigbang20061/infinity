//! The new player experience (storylines.client.airnpe, nodegraph.client).
//!
//! This build's opening tutorial ("AIR NPE") is a set of node graphs: server graph 49
//! ([AIR] 0.0 - Server Master) with ~40 server subgraphs, and client graph 716 with ~88
//! client subgraphs (both in sde_native.nodegraphs). The server graphs spawn the
//! tutorial's dungeons (9103 station wreckage, 9105, the mining dungeon), allocate a
//! void-space pocket, and script the guide NPC and enemy waves through dungeon behavior
//! blackboards. Those dungeon layouts and behavior trees never shipped with the client
//! or the SDE, so the tutorial can't be run: `is_air_npe_enabled` stays off and new
//! characters start in their school station as if they had skipped it.
//!
//! What the client still needs is each character's tutorial state: anything but
//! COMPLETED or SKIPPED reads as "tutorial unfinished", which hides the HUD's lock-and-fire
//! buttons and the amplification matrix slot (shipUI, hudButtonsCont).

mod air_npe;
mod node_graph;

use super::{Call, CallResult};
use crate::client::Client;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "air_npe" => air_npe::dispatch(c, call).await,
        "node_graph" => node_graph::dispatch(c, call),
        _ => None,
    }
}
