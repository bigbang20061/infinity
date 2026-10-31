//! Bound skillHandler calls; no specific client module path is established.
//! Uses game.skills, game.skill_queue, game.characters and startup SDE data
//! through the training and attribute helpers.
//! History, boosters, implants, penalties and several other calls use fixed answers.
//! The reported free respec count is an operator policy.
use super::*;
use super::attributes::character_attributes;
use super::training::{skills_dict, queue_list, save_queue};
use crate::client::{filetime_now, short};
use marshal::Value;
use tracing::warn;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let r = match (call.service, call.method) {
        (SKILL_HANDLER, "GetSkills") | (SKILL_HANDLER, "GetAllSkills") => skills_dict(c, me).await,
        // [(typeID, pointChange)] since the last login
        (SKILL_HANDLER, "GetSkillChangesForISIS") => Ok(Value::list([])),
        (SKILL_HANDLER, "CheckAndSendNotifications") => Ok(Value::None),
        (SKILL_HANDLER, "GetSkillQueueAndFreePoints") => {
            let free: i64 = match crate::dao::characters::attributes::free_points(&c.db, me).await {
                Ok(points) => points,
                Err(e) => return Some(Err(db_err(e))),
            };
            queue_list(c, me).await.map(|q| Value::tuple([q, Value::Int(free)]))
        }
        (SKILL_HANDLER, "GetSkillQueue") => queue_list(c, me).await,
        (SKILL_HANDLER, "SaveNewQueue") => {
            let activate = call.kwargs.get("activate").or_else(|| call.args.get(1)).map(|v| v.as_int() != Some(0)).unwrap_or(true);
            save_queue(c, me, call.args.first().unwrap_or(&Value::None), activate, true).await.map(|()| Value::None)
        }
        (SKILL_HANDLER, "AbortTraining") => match save_queue(c, me, &Value::Dict(vec![]), false, false).await {
            Ok(()) => Ok(Value::None),
            Err(e) => Err(e),
        },
        (SKILL_HANDLER, "GetFreeSkillPoints") => {
            match crate::dao::characters::attributes::free_points(&c.db, me).await {
                Ok(points) => Ok(Value::Int(points)),
                Err(e) => Err(db_err(e)),
            }
        }
        (SKILL_HANDLER, "GetSkillPoints") => match crate::dao::skills::training::total(&c.db, me).await
        {
            Ok(points) => Ok(Value::Int(points)),
            Err(e) => Err(db_err(e)),
        },
        (SKILL_HANDLER, "GetAttributes") => character_attributes(c, me)
            .await
            .map(|a| Value::Dict(a.into_iter().map(|(k, v)| (Value::Int(k), Value::Float(v))).collect())),
        (SKILL_HANDLER, "GetBoosters") | (SKILL_HANDLER, "GetImplants") => Ok(Value::Dict(vec![])),
        (SKILL_HANDLER, "GetSkillHistory") => Ok(Value::list([])),
        (SKILL_HANDLER, "GetPenaltySkillPoints") | (SKILL_HANDLER, "GetDiminishedSpFromInjectors") => Ok(Value::Int(0)),
        (SKILL_HANDLER, "GetRespecInfo") => Ok(keyval([
            ("freeRespecs", Value::Int(crate::config::game().characters.free_respecs)),
            ("lastRespecDate", Value::None),
            ("nextTimedRespec", Value::Int(filetime_now())),
        ])),
        (SKILL_HANDLER, m) => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED skillHandler::{m} args={} kwargs={}",
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
        _ => return None,
    };
    Some(r)
}