//! Durable, fenced mutual-war and invitation deadlines, mirroring
//! `eve/client/script/ui/services/wars/mutualWarInvites.py` and `evewar/const.py`.
//! Uses `game.jobs`, `game.wars`, `game.war_invites` and `game.war_invite_settings`.
//! Stale jobs do not notify; aggressive wars, allies and paid treaties are unsupported.
use crate::{client::filetime_now, dao::wars::{self, Invite, War}, db::{Db, executor::Executor, outbox::{Outbox, dispatch_world}}, scheduler::{self, Job}, services::CallError, space::World};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
use super::lifecycle;

pub(crate) const KIND: &str = "wars.deadline";
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Deadline { Invite { from: i64, to: i64, sent: i64, due: i64 }, Start { id: i64, due: i64 }, Finish { id: i64, due: i64 } }
fn invite_id(from: i64, to: i64) -> String { format!("{KIND}:invite:{from}:{to}") }
async fn schedule(ex: &(impl Executor + ?Sized), id: String, due: i64, deadline: Deadline) -> Result<(), CallError> {
    let job = Job { id, kind: KIND.into(), due_at: due, payload: serde_json::to_string(&deadline).map_err(crate::services::db_err)? };
    scheduler::schedule(ex, &job).await?;
    Ok(())
}
pub(super) async fn schedule_invite(ex: &(impl Executor + ?Sized), invite: &Invite) -> Result<(), CallError> {
    schedule(ex, invite_id(invite.from_owner, invite.to_owner), invite.expires_at, Deadline::Invite { from: invite.from_owner, to: invite.to_owner, sent: invite.sent_at, due: invite.expires_at }).await
}
pub(super) async fn cancel_invite(ex: &(impl Executor + ?Sized), from: i64, to: i64) -> Result<(), CallError> { scheduler::cancel(ex, &invite_id(from,to)).await?; Ok(()) }
pub(super) async fn schedule_start(ex: &(impl Executor + ?Sized), war: &War) -> Result<(), CallError> {
    schedule(ex, format!("{KIND}:start:{}", war.war_id), war.starts_at, Deadline::Start { id: war.war_id, due: war.starts_at }).await
}
pub(super) async fn schedule_finish(ex: &(impl Executor + ?Sized), war: &War) -> Result<(), CallError> {
    let due = war.finishes_at.ok_or_else(lifecycle::invalid)?;
    schedule(ex, format!("{KIND}:finish:{}", war.war_id), due, Deadline::Finish { id: war.war_id, due }).await
}
pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let deadline: Deadline = serde_json::from_str(&job.payload)?;
        db.transaction(|tx, outbox: &mut Outbox| {
            let deadline = deadline;
            Box::pin(async move {
                let now = filetime_now();
                match deadline {
                    Deadline::Invite { from, to, sent, due } => {
                        wars::lock_owners(tx, &[from,to]).await?;
                        if wars::expire_invite(tx, from, to, sent, due, now).await? { lifecycle::invites_changed(tx, outbox, from, to).await?; }
                    }
                    Deadline::Start { id, due } | Deadline::Finish { id, due } => {
                        if let Some(original) = wars::get(tx, id).await? {
                            wars::lock_owners(tx, &[original.declared_by, original.against]).await?;
                            let start = matches!(deadline, Deadline::Start { .. });
                            let changed = if start { wars::activate(tx, id, due, now).await? } else { wars::finish(tx, id, due, now).await? };
                            if let Some(war) = changed {
                                lifecycle::war_changed(tx, outbox, &war, marshal::Value::Dict(vec![])).await?;
                            }
                        }
                    }
                }
                Ok::<(), CallError>(())
            })
        }, |value, outbox| async move { dispatch_world(value, outbox, &world) }).await.map_err(|e| anyhow::anyhow!("war deadline failed: {e:?}"))
    })
}
