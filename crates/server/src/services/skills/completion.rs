//! Durable skill-queue completion for characterskills/util.py.
//! Uses game.skill_queue, game.skills and game.jobs; queue timing uses effective
//! Fit attributes. Completion and manual changes share one fresh-state batch and
//! pilot brain refresh; notifications and Lua events follow commit.
use super::{db_err, training::{complete_tx, queue_list_db, skill_info}, CallError};
use crate::{client::{filetime_now, BRAIN_REFRESH}, dao::skills::{queue, training}, db::{executor::Executor, outbox::{dispatch_world, Outbox}, Db}, scheduler::{self, Job}, space::World};
use marshal::Value;
use std::{future::Future, pin::Pin, sync::Arc};

pub(crate) const KIND: &str = "skills.complete";

pub(super) async fn reschedule(tx: &(impl Executor + ?Sized), char_id: i64) -> Result<(), CallError> {
    let id = format!("{KIND}:{char_id}");
    if let Some(due_at) = queue::active(tx, char_id).await.map_err(db_err)?.and_then(|row| row.end_time) {
        let job = Job { id, kind: KIND.into(), due_at, payload: char_id.to_string() };
        scheduler::schedule(tx, &job).await.map_err(db_err)?;
    } else {
        scheduler::cancel(tx, &id).await.map_err(db_err)?;
    }
    Ok(())
}

pub(crate) async fn seed(db: &Db) -> Result<(), CallError> {
    for char_id in queue::active_characters(db).await.map_err(db_err)? {
        db.transaction(|tx, _outbox: &mut Outbox| Box::pin(async move {
            crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
            reschedule(tx, char_id).await
        }), |value, outbox| async move { let _ = outbox; Ok(value) }).await?;
    }
    scheduler::wake();
    Ok(())
}

pub(super) async fn notify_changes<E>(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, sde: &crate::sde::Sde, char_id: i64, now: i64, finished: &[(i32, i32)], additional: &[i32]) -> Result<(), CallError> {
    if finished.is_empty() && additional.is_empty() { return Ok(()); }
    let types: std::collections::BTreeSet<_> = finished.iter().map(|(type_id, _)| *type_id).chain(additional.iter().copied()).collect();
    let infos = training::current(tx, char_id).await.map_err(db_err)?.into_iter()
        .filter(|skill| types.contains(&skill.type_id))
        .map(|skill| (Value::Int(skill.type_id as i64), skill_info(sde, skill.type_id, skill.level, skill.skill_points, now))).collect();
    outbox.notify_pilot(char_id, "OnServerSkillsChanged", vec![Value::Dict(infos), Value::None, Value::Int(now)]);
    outbox.notify_pilot(char_id, BRAIN_REFRESH, vec![]);
    Ok(())
}

pub(crate) fn handle(db: Db, world: Arc<World>, job: Job) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        let char_id: i64 = serde_json::from_str(&job.payload)?;
        let now = filetime_now();
        let result = db.transaction(|tx, outbox: &mut Outbox| {
            let sde = world.sde().clone();
            Box::pin(async move {
                crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
                let finished = complete_tx(tx, outbox, char_id, now).await?;
                notify_changes(tx, outbox, &sde, char_id, now, &finished, &[]).await?;
                if !finished.is_empty() {
                    outbox.notify_pilot(char_id, "OnNewSkillQueueSaved", vec![queue_list_db(tx, char_id).await?]);
                }
                reschedule(tx, char_id).await
            })
        }, |value, outbox| {
            let world = &world;
            async move { let result = dispatch_world(value, outbox, world); scheduler::wake(); result }
        }).await;
        result.map_err(|error| match error {
            CallError::Internal(error) => error,
            CallError::User { msg, .. } => anyhow::anyhow!("skill completion refused: {msg}"),
        })
    })
}

#[cfg(test)] mod tests;
