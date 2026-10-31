//! Skill training calculations and queue policy for characterskills/util.py.
//! Uses game.skills, game.skill_queue, game.characters and startup SDE data;
//! missing ranks and attributes use server fallbacks.
//! Finished levels are committed by the durable completion scheduler or mutations;
//! training rates use effective character attributes computed by Fit.
//! SaveNewQueue validates whole queues against skillQueueSvc.py:186-234,259-263.
//! Malformed maps use QueueInvalidPosition; levels must be consecutive (server
//! policy preventing skipped levels). For a dependency found later in the saved
//! queue, the whole-queue check reports QueueCannotPlaceSkillAfterDependentSkills.
//! Clone-grade restrictions and the client queue time limit are not enforced.
use super::*;
use super::attributes::{character_attributes_db, CHAR_ATTRIBUTES};
use super::formulas::sp_for_level;
use crate::client::filetime_now;
use crate::consts::{CATEGORY_SKILL, FILETIME_MINUTE, MAX_SKILL_LEVEL};
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_client, Outbox};
use crate::dao::skills::{queue as queue_dao, training as training_dao};
use marshal::Value;
use std::collections::{BTreeMap, HashMap};
use tracing::info;

/// SDE dgmAttributeTypes: primaryAttribute.
const ATTR_PRIMARY: i32 = 180;
/// SDE dgmAttributeTypes: secondaryAttribute.
const ATTR_SECONDARY: i32 = 181;
/// SDE dgmAttributeTypes: skillTimeConstant.
const ATTR_RANK: i32 = 275;
/// Server fallback rank when an SDE skill rank attribute is missing.
const FALLBACK_SKILL_RANK: f64 = 1.0;
/// Server fallback for missing training attributes.
const FALLBACK_TRAINING_ATTRIBUTE: f64 = 20.0;
/// Client characterskills/util.py secondary attribute divisor.
const SECONDARY_ATTRIBUTE_DIVISOR: f64 = 2.0;
/// Client characterskills/queue.py: SKILLQUEUE_MAX_NUM_SKILLS.
const MAX_QUEUE_ENTRIES: usize = 150;
/// Client const.UE_TYPEID UserError substitution tag.
const UE_TYPEID: i64 = 4;

fn refusal(msg: &'static str) -> CallError {
    CallError::User { msg: msg.into(), dict: vec![] }
}

fn parse_queue(queue: &Value) -> Result<Vec<(i64, i64, i64)>, CallError> {
    let Value::Dict(pairs) = queue else { return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION)) };
    if pairs.len() > MAX_QUEUE_ENTRIES {
        return Err(CallError::User { msg: crate::messages::MSG_QUEUE_TOO_MANY.into(), dict: vec![("num".into(), Value::Int(MAX_QUEUE_ENTRIES as i64))] });
    }
    let mut entries = Vec::with_capacity(pairs.len());
    for (key, value) in pairs {
        let Value::Int(position) = key else { return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION)) };
        let (Value::Tuple(values) | Value::List(values)) = value else { return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION)) };
        let [Value::Int(type_id), Value::Int(level)] = values.as_slice() else { return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION)) };
        if *position < 0 { return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION)); }
        entries.push((*position, *type_id, *level));
    }
    entries.sort_unstable_by_key(|entry| entry.0);
    if entries.iter().enumerate().any(|(index, entry)| entry.0 != index as i64) {
        return Err(refusal(crate::messages::MSG_QUEUE_INVALID_POSITION));
    }
    Ok(entries)
}

fn validate_queue(sde: &crate::sde::Sde, entries: &[(i64, i64, i64)], current: &HashMap<i32, (i32, i64)>) -> Result<(), CallError> {
    use crate::messages as m;
    if entries.len() > MAX_QUEUE_ENTRIES {
        return Err(CallError::User { msg: m::MSG_QUEUE_TOO_MANY.into(), dict: vec![("num".into(), Value::Int(MAX_QUEUE_ENTRIES as i64))] });
    }
    let mut levels = HashMap::new();
    for (index, &(position, raw_type, raw_level)) in entries.iter().enumerate() {
        if position != index as i64 { return Err(refusal(m::MSG_QUEUE_INVALID_POSITION)); }
        let type_id = i32::try_from(raw_type).map_err(|_| refusal(m::MSG_QUEUE_SKILL_NOT_UPLOADED))?;
        if !sde.types.get(type_id).is_some_and(|t| t.category_id == Some(CATEGORY_SKILL)) {
            return Err(refusal(m::MSG_QUEUE_SKILL_NOT_UPLOADED));
        }
        let &(trained, points) = current.get(&type_id).ok_or_else(|| refusal(m::MSG_QUEUE_SKILL_NOT_UPLOADED))?;
        if raw_level <= i64::from(trained) { return Err(refusal(m::MSG_QUEUE_PREVIOUSLY_TRAINED)); }
        if raw_level > i64::from(MAX_SKILL_LEVEL) {
            return Err(CallError::User { msg: m::MSG_QUEUE_PAST_MAXIMUM.into(), dict: vec![("typeName".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(raw_type)]))] });
        }
        let level = raw_level as i32;
        let previous = levels.get(&type_id).copied().unwrap_or(trained);
        if level <= previous { return Err(refusal(m::MSG_QUEUE_ALREADY_PRESENT)); }
        if level != previous + 1 { return Err(refusal(m::MSG_QUEUE_LEVELS_OUT_OF_ORDER)); }
        if points == 0 {
            for (required, required_level) in super::requirements::requirements(sde, type_id) {
                let Some(&(trained_required, _)) = current.get(&required) else { return Err(refusal(m::MSG_QUEUE_BEFORE_REQUIREMENTS)) };
                if trained_required.max(levels.get(&required).copied().unwrap_or(0)) < required_level {
                    let later = entries[index + 1..].iter().any(|&(_, t, l)| t == i64::from(required) && l == i64::from(required_level));
                    return Err(refusal(if later { m::MSG_QUEUE_AFTER_DEPENDENTS } else { m::MSG_QUEUE_BEFORE_REQUIREMENTS }));
                }
            }
        }
        levels.insert(type_id, level);
    }
    Ok(())
}

pub(super) struct SkillType {
    pub(super) rank: f64,
    primary: i64,
    secondary: i64,
}

pub(super) fn skill_type(sde: &crate::sde::Sde, type_id: i32) -> SkillType {
    let attribute = |id| sde.types.attribute(type_id, id);
    SkillType {
        rank: attribute(ATTR_RANK).unwrap_or(FALLBACK_SKILL_RANK),
        primary: attribute(ATTR_PRIMARY).map(|v| v as i64).unwrap_or(CHAR_ATTRIBUTES[1]),
        secondary: attribute(ATTR_SECONDARY).map(|v| v as i64).unwrap_or(CHAR_ATTRIBUTES[2]),
    }
}
pub(super) fn skill_info(sde: &crate::sde::Sde, type_id: i32, level: i32, points: i64, now: i64) -> Value {
    keyval([
        ("typeID", Value::Int(type_id as i64)),
        ("trainedSkillLevel", Value::Int(level as i64)),
        ("effectiveSkillLevel", Value::Int(level as i64)),
        ("virtualSkillLevel", Value::None),
        ("trainedSkillPoints", Value::Int(points)),
        ("skillPoints", Value::Int(points)),
        ("skillLevel", Value::Int(level as i64)),
        ("skillRank", Value::Float(skill_type(sde, type_id).rank)),
        ("timeStamp", Value::Int(now)),
    ])
}


// None level means banking only: it must not create a missing skill row.
pub(super) fn plan_skills(done: &[(i32, i32, i64)], training: Option<(i32, i64)>) -> BTreeMap<i32, (Option<i32>, i64)> {
    let mut plan = BTreeMap::new();
    for &(type_id, level, sp) in done {
        plan.entry(type_id)
            .and_modify(|(old_level, old_sp): &mut (Option<i32>, i64)| {
                *old_level = Some(old_level.unwrap_or(level).max(level));
                *old_sp = (*old_sp).max(sp);
            })
            .or_insert((Some(level), sp));
    }
    if let Some((type_id, sp)) = training {
        plan.entry(type_id).and_modify(|(_, old_sp)| *old_sp = (*old_sp).max(sp)).or_insert((None, sp));
    }
    plan
}
fn completed_levels(done: &[(i32, i32, i64)], mut levels: HashMap<i32, i32>) -> Vec<(i32, i32)> {
    let mut entries: Vec<_> = done.iter().map(|&(type_id, level, _)| (type_id, level)).collect();
    entries.sort_unstable();
    entries.retain(|&(type_id, level)| {
        let current = levels.entry(type_id).or_insert(0);
        if level <= *current { return false; }
        *current = level;
        true
    });
    entries
}

pub(super) fn queue_training_events<X>(outbox: &mut Outbox<X>, char_id: i64, finished: &[(i32, i32)]) {
    for &(type_id, level) in finished {
        outbox.event(crate::lua::events::GameEvent::SkillTrained { char_id, type_id, level });
    }
}

/// Complete due rows in the caller's serialized mutation transaction.
pub(super) async fn complete_tx<E>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<E>, char_id: i64, now: i64) -> Result<Vec<(i32, i32)>, CallError> {
    let levels = training_dao::current(db, char_id).await.map_err(db_err)?.into_iter().map(|r| (r.type_id, r.level)).collect();
    let rows = queue_dao::collect_finished(db, char_id, now).await.map_err(db_err)?;
    let done: Vec<_> = rows.iter().map(|r| (r.type_id, r.to_level, r.dest_sp)).collect();
    training_dao::write_skills(db, char_id, &plan_skills(&done, None)).await.map_err(db_err)?;
    let finished = completed_levels(&done, levels);
    for &(type_id, level) in &finished {
        if let Some(row) = rows.iter().find(|r| r.type_id == type_id && r.to_level == level) {
            crate::dao::skills::history::append(db, char_id, type_id, super::history::TRAINING_COMPLETE, row.dest_sp, row.end_time).await.map_err(db_err)?;
        }
    }
    queue_training_events(outbox, char_id, &finished);
    Ok(finished)
}

/// SP accrued so far by the entry currently in training, if any.
pub(super) async fn in_training_sp(db: &(impl Executor + ?Sized), char_id: i64, now: i64) -> Result<Option<(i32, i64)>, CallError> {
    let r = queue_dao::active(db, char_id).await.map_err(db_err)?;
    Ok(r.and_then(|r| {
        let (start, end) = (r.start_time?, r.end_time?);
        let frac = ((now - start) as f64 / (end - start).max(1) as f64).clamp(0.0, 1.0);
        Some((r.type_id, r.start_sp + ((r.dest_sp - r.start_sp) as f64 * frac) as i64))
    }))
}

pub(super) async fn skills_dict(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    let now = filetime_now();
    let training = in_training_sp(&c.db, char_id, now).await?;
    let rows = training_dao::current(&c.db, char_id).await.map_err(db_err)?;
    let now = filetime_now();
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let (type_id, level, mut sp) = (r.type_id, r.level, r.skill_points);
                if let Some((t, trained)) = training {
                    if t == type_id { sp = sp.max(trained); }
                }
                (Value::Int(type_id as i64), skill_info(&c.sde, type_id, level, sp, now))
            })
            .collect(),
    ))
}

pub(super) async fn queue_list(c: &mut Client, char_id: i64) -> Result<Value, CallError> {
    queue_list_db(&c.db, char_id).await
}

pub(super) async fn queue_list_db(db: &(impl Executor + ?Sized), char_id: i64) -> Result<Value, CallError> {
    let rows = queue_dao::list(db, char_id).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::List(
        rows.iter()
            .map(|r| {
                keyval([
                    ("queuePosition", Value::Int(r.position as i64)),
                    ("trainingTypeID", Value::Int(r.type_id as i64)),
                    ("trainingToLevel", Value::Int(r.to_level as i64)),
                    ("trainingStartSP", Value::Int(r.start_sp)),
                    ("trainingDestinationSP", Value::Int(r.dest_sp)),
                    ("trainingStartTime", opt(r.start_time)),
                    ("trainingEndTime", opt(r.end_time)),
                ])
            })
            .collect(),
    ))
}

pub(super) async fn save_queue_tx(tx: &(impl Executor + ?Sized), outbox: &mut Outbox, sde: &std::sync::Arc<crate::sde::Sde>, char_id: i64, entries: Vec<(i64, i64, i64)>, activate: bool, notify: bool, now: i64, mut finished: Vec<(i32, i32)>, additional: &[i32]) -> Result<Vec<(i32, i32)>, CallError> {
    crate::dao::characters::selection::lock_character(tx, char_id).await.map_err(db_err)?;
    finished.extend(complete_tx(tx, outbox, char_id, now).await?);
    let training = in_training_sp(tx, char_id, now).await?;
    training_dao::write_skills(tx, char_id, &plan_skills(&[], training)).await.map_err(db_err)?;
    let current: HashMap<i32, (i32, i64)> = training_dao::current(tx, char_id).await.map_err(db_err)?.into_iter().map(|r| (r.type_id, (r.level, r.skill_points))).collect();
    validate_queue(sde, &entries, &current)?;
    let attrs = character_attributes_db(tx, sde, char_id).await?;
    queue_dao::clear(tx, char_id).await.map_err(db_err)?;
    let mut t = now;
    for (pos, (_, type_id, to_level)) in entries.into_iter().enumerate() {
        let st = skill_type(&sde, type_id as i32);
        let (cur_level, cur_sp) = current.get(&(type_id as i32)).copied().unwrap_or((0, 0));
        let start_sp = if cur_level as i64 == to_level - 1 { cur_sp.max(sp_for_level(st.rank, to_level - 1)) } else { sp_for_level(st.rank, to_level - 1) };
        let dest_sp = sp_for_level(st.rank, to_level);
        let rate = attrs.get(&st.primary).copied().unwrap_or(FALLBACK_TRAINING_ATTRIBUTE) + attrs.get(&st.secondary).copied().unwrap_or(FALLBACK_TRAINING_ATTRIBUTE) / SECONDARY_ATTRIBUTE_DIVISOR;
        let (start, end) = if activate {
            let end = t + (((dest_sp - start_sp).max(0) as f64 / rate) * FILETIME_MINUTE as f64) as i64;
            let s = t;
            t = end;
            (Some(s), Some(end))
        } else {
            (None, None)
        };
        queue_dao::insert(tx, char_id, &queue_dao::QueueEntry { position: pos as i32, type_id: type_id as i32, to_level: to_level as i32, start_sp, dest_sp, start_time: start, end_time: end }).await.map_err(db_err)?;
    }
    if notify {
        finished.extend(complete_tx(tx, outbox, char_id, now).await?);
    }
    super::completion::reschedule(tx, char_id).await?;
    super::completion::notify_changes(tx, outbox, sde, char_id, now, &finished, additional).await?;
    if notify {
        outbox.notify_client("OnNewSkillQueueSaved", vec![queue_list_db(tx, char_id).await?]);
    }
    Ok(finished)
}
/// `SaveNewQueue({idx: (typeID, toLevel)}, activate=bool)`: rebuild the queue with timings.
pub(super) async fn save_queue(c: &mut Client, char_id: i64, queue: &Value, activate: bool, notify: bool) -> Result<(), CallError> {
    let now = filetime_now();
    let entries = parse_queue(queue)?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let entries = entries.clone();
            let sde = sde.clone();
            Box::pin(async move {
                save_queue_tx(tx, outbox, &sde, char_id, entries, activate, notify, now, vec![], &[]).await
            })
        },
        |finished, outbox| async move {
            for &(type_id, level) in &finished {
                info!("character {char_id} finished training skill {type_id} to level {level}");
            }
            crate::scheduler::wake();
            dispatch_client((), outbox, c)?;
            Ok(())
        },
    ).await
}

#[cfg(test)] mod tests;
