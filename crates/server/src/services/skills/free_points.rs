//! Applying unallocated skill points for skillsvc.py.
//! Uses game.characters, skills, history, queue and jobs; changes dispatch after commit.
use super::{db_err, training, CallError};
use crate::{client::{filetime_now, Client}, consts::dogma::MAX_SKILL_LEVEL, dao::{characters::attributes, skills::{history, queue, trading, training as skill_dao}}, db::{executor::Executor, outbox::Outbox}, messages, services::{effects::online::{self, OnlineChanged}, notify_error}};
use marshal::Value;

fn refusal() -> CallError { notify_error(messages::skills::INVALID_ECONOMY_REQUEST) }

async fn apply_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox<OnlineChanged>, sde: &std::sync::Arc<crate::sde::Sde>, char_id: i64, type_id: i32, points: i64, now: i64) -> Result<Value, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    let progress = super::completion::advance_tx(tx, outbox, sde, char_id, now).await?;
    let accrued = training::in_training_sp(tx, char_id, now).await?;
    skill_dao::write_skills(tx, char_id, &training::plan_skills(&[], accrued)).await.map_err(db_err)?;
    sde.types.get(type_id).filter(|t| t.category_id == Some(crate::consts::CATEGORY_SKILL) && t.published == Some(true)).ok_or_else(refusal)?;
    let skill = skill_dao::current(tx, char_id).await.map_err(db_err)?.into_iter().find(|s| s.type_id == type_id).ok_or_else(refusal)?;
    let free = attributes::free_points(tx, char_id).await.map_err(db_err)?;
    let new_sp = skill.skill_points.checked_add(points).ok_or_else(refusal)?;
    let rank = training::skill_type(sde, type_id).rank;
    if points <= 0 || points > free || new_sp > super::formulas::sp_for_level(rank, i64::from(MAX_SKILL_LEVEL)) { return Err(refusal()); }
    let level = super::formulas::level_from_points(new_sp, rank);
    if !attributes::replace_free_points(tx, char_id, free, free - points).await.map_err(db_err)? || !trading::replace_skill(tx, char_id, type_id, skill.level, skill.skill_points, level, new_sp).await.map_err(db_err)? { return Err(refusal()); }
    history::append(tx, char_id, type_id, super::history::FREE_POINTS_USED, new_sp, now).await.map_err(db_err)?;
    let rows = queue::list(tx, char_id).await.map_err(db_err)?;
    let activate = rows.first().is_some_and(|row| row.start_time.is_some());
    let entries = rows.into_iter().filter(|row| row.type_id != type_id || row.to_level > level).enumerate().map(|(pos, row)| (pos as i64, i64::from(row.type_id), i64::from(row.to_level))).collect();
    training::save_queue_tx(tx, outbox, sde, char_id, entries, activate, true, now, progress, &[type_id]).await?;
    outbox.notify_client("OnFreeSkillPointsChanged", vec![Value::Int(free - points)]);
    Ok(Value::Int(free - points))
}

pub(super) async fn apply(c: &mut Client, char_id: i64, args: &[Value]) -> Result<Value, CallError> {
    let type_id = args.first().and_then(Value::as_int).and_then(|v| i32::try_from(v).ok()).ok_or_else(refusal)?;
    let points = args.get(1).and_then(Value::as_int).ok_or_else(refusal)?;
    let now = filetime_now();
    let _lock = crate::services::market::lock().await;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(|tx, outbox: &mut Outbox<OnlineChanged>| { let sde = sde.clone(); Box::pin(async move { apply_tx(tx, outbox, &sde, char_id, type_id, points, now).await }) }, |value, outbox| async move { crate::scheduler::wake(); online::dispatch_changed(value, outbox, c) }).await
}

#[cfg(test)] mod tests;
