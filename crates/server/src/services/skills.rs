//! Skills and skill training (skillMgr2 / the bound skill handler).
//! Formulas match characterskills/util.py: SP(level) = ceil(250 * rank * 2^(2.5(level-1))),
//! SP/minute = primary + secondary / 2.

use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, short, Client};
use crate::consts::FILETIME_MINUTE;
use marshal::Value;
use std::collections::HashMap;
use tracing::{info, warn};

/// Internal bound-object name for skillMgr2.GetMySkillHandler.
pub const SKILL_HANDLER: &str = "skillHandler";

/// SDE dgmAttributeTypes: primaryAttribute.
const ATTR_PRIMARY: i32 = 180;
/// SDE dgmAttributeTypes: secondaryAttribute.
const ATTR_SECONDARY: i32 = 181;
/// SDE dgmAttributeTypes: skillTimeConstant.
const ATTR_RANK: i32 = 275;
/// Server fallback rank when an SDE skill rank attribute is missing.
const FALLBACK_SKILL_RANK: f64 = 1.0;
/// SDE dgmAttributeTypes: charisma, intelligence, memory, perception, willpower.
const CHAR_ATTRIBUTES: [i64; 5] = [164, 165, 166, 167, 168];
/// Client characterskills/util.py skill point formula base.
const SKILL_POINT_BASE: f64 = 250.0;
/// Client characterskills/util.py skill point formula exponential base.
const SKILL_LEVEL_BASE: f64 = 2.0;
/// Client characterskills/util.py skill point formula exponent multiplier.
const SKILL_LEVEL_EXPONENT: f64 = 2.5;
/// Client characterskills/util.py maximum trained skill level.
const MAX_SKILL_LEVEL: i64 = 5;
/// Server fallback for missing SDE bloodline attributes.
const FALLBACK_BLOODLINE_ATTRIBUTE: i32 = 5;
/// Server character attribute calculation offset.
const CHARACTER_ATTRIBUTE_OFFSET: f64 = 17.0;
/// Server fallback for missing training attributes.
const FALLBACK_TRAINING_ATTRIBUTE: f64 = 20.0;
/// Client characterskills/util.py secondary attribute divisor.
const SECONDARY_ATTRIBUTE_DIVISOR: f64 = 2.0;

pub fn sp_for_level(rank: f64, level: i64) -> i64 {
    if level <= 0 {
        return 0;
    }
    (SKILL_POINT_BASE * rank * SKILL_LEVEL_BASE.powf(SKILL_LEVEL_EXPONENT * (level.min(MAX_SKILL_LEVEL) - 1) as f64)).ceil() as i64
}

/// Character attribute values {attributeID: value}.
pub async fn character_attributes(c: &Client, char_id: i64) -> Result<HashMap<i64, f64>, CallError> {
    let r = c
        .db
        .query_one(
            "SELECT b.charisma, b.intelligence, b.memory, b.perception, b.willpower
             FROM game.characters ch JOIN sde.chr_bloodlines b USING (bloodline_id) WHERE ch.character_id = $1",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    Ok(CHAR_ATTRIBUTES.iter().enumerate().map(|(i, a)| (*a, r.get::<_, Option<i32>>(i).unwrap_or(FALLBACK_BLOODLINE_ATTRIBUTE) as f64 + CHARACTER_ATTRIBUTE_OFFSET)).collect())
}

struct SkillType {
    rank: f64,
    primary: i64,
    secondary: i64,
}

async fn skill_type(c: &Client, type_id: i32) -> Result<SkillType, CallError> {
    let rows = c
        .db
        .query(
            "SELECT attribute_id, value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = ANY($2)",
            &[&type_id, &vec![ATTR_PRIMARY, ATTR_SECONDARY, ATTR_RANK]],
        )
        .await
        .map_err(db_err)?;
    let mut t = SkillType { rank: FALLBACK_SKILL_RANK, primary: CHAR_ATTRIBUTES[1], secondary: CHAR_ATTRIBUTES[2] };
    for r in rows {
        let v: f64 = r.get(1);
        match r.get::<_, i32>(0) {
            ATTR_RANK => t.rank = v,
            ATTR_PRIMARY => t.primary = v as i64,
            ATTR_SECONDARY => t.secondary = v as i64,
            _ => {}
        }
    }
    Ok(t)
}

/// Grant the race's starting skills to a new character (sde chr_races.skills).
pub async fn grant_starting_skills(c: &Client, char_id: i64, race_id: i32) -> Result<(), CallError> {
    let rows = c
        .db
        .query(
            "SELECT (s->>'_key')::int, (s->>'_value')::int FROM sde.chr_races r, jsonb_array_elements(r.data->'skills') s WHERE r.race_id = $1",
            &[&race_id],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let (type_id, level): (i32, i32) = (r.get(0), r.get(1));
        let rank = skill_type(c, type_id).await?.rank;
        c.db
            .execute(
                "INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                &[&char_id, &type_id, &level, &sp_for_level(rank, level as i64)],
            )
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

/// Apply training progress up to now: finished entries become trained levels.
async fn advance_training(c: &Client, char_id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    let done = c
        .db
        .query(
            "DELETE FROM game.skill_queue WHERE character_id = $1 AND end_time IS NOT NULL AND end_time <= $2
             RETURNING type_id, to_level, dest_sp",
            &[&char_id, &now],
        )
        .await
        .map_err(db_err)?;
    for r in &done {
        let (type_id, level, sp): (i32, i32, i64) = (r.get(0), r.get(1), r.get(2));
        c.db
            .execute(
                "INSERT INTO game.skills (character_id, type_id, level, skill_points) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (character_id, type_id) DO UPDATE SET level = GREATEST(game.skills.level, EXCLUDED.level),
                     skill_points = GREATEST(game.skills.skill_points, EXCLUDED.skill_points)",
                &[&char_id, &type_id, &level, &sp],
            )
            .await
            .map_err(db_err)?;
        info!("character {char_id} finished training skill {type_id} to level {level}");
    }
    if !done.is_empty() {
        c.db
            .execute(
                "UPDATE game.skill_queue q SET position = n.rn - 1 FROM
                    (SELECT position, row_number() OVER (ORDER BY position) rn FROM game.skill_queue WHERE character_id = $1) n
                 WHERE q.character_id = $1 AND q.position = n.position",
                &[&char_id],
            )
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

/// SP accrued so far by the entry currently in training, if any.
async fn in_training_sp(c: &Client, char_id: i64) -> Result<Option<(i32, i64)>, CallError> {
    let r = c
        .db
        .query_opt(
            "SELECT type_id, start_sp, dest_sp, start_time, end_time FROM game.skill_queue WHERE character_id = $1 AND position = 0",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    Ok(r.and_then(|r| {
        let (start_sp, dest_sp): (i64, i64) = (r.get(1), r.get(2));
        let (start, end): (Option<i64>, Option<i64>) = (r.get(3), r.get(4));
        let (start, end) = (start?, end?);
        let frac = ((filetime_now() - start) as f64 / (end - start).max(1) as f64).clamp(0.0, 1.0);
        Some((r.get::<_, i32>(0), start_sp + ((dest_sp - start_sp) as f64 * frac) as i64))
    }))
}

async fn skills_dict(c: &Client, char_id: i64) -> Result<Value, CallError> {
    advance_training(c, char_id).await?;
    let training = in_training_sp(c, char_id).await?;
    let rows = c
        .db
        .query(
            "SELECT s.type_id, s.level, s.skill_points, COALESCE(a.value, $3::float8)
             FROM game.skills s LEFT JOIN sde.type_attributes a ON a.type_id = s.type_id AND a.attribute_id = $2
             WHERE s.character_id = $1",
            &[&char_id, &ATTR_RANK, &FALLBACK_SKILL_RANK],
        )
        .await
        .map_err(db_err)?;
    let now = filetime_now();
    Ok(Value::Dict(
        rows.iter()
            .map(|r| {
                let (type_id, level, mut sp, rank): (i32, i32, i64, f64) = (r.get(0), r.get(1), r.get(2), r.get(3));
                if let Some((t, trained)) = training {
                    if t == type_id {
                        sp = sp.max(trained);
                    }
                }
                let lvl = Value::Int(level as i64);
                (
                    Value::Int(type_id as i64),
                    keyval([
                        ("typeID", Value::Int(type_id as i64)),
                        ("trainedSkillLevel", lvl.clone()),
                        ("effectiveSkillLevel", lvl.clone()),
                        ("virtualSkillLevel", Value::None),
                        ("trainedSkillPoints", Value::Int(sp)),
                        ("skillPoints", Value::Int(sp)),
                        ("skillLevel", lvl),
                        ("skillRank", Value::Float(rank)),
                        ("timeStamp", Value::Int(now)),
                    ]),
                )
            })
            .collect(),
    ))
}

async fn queue_list(c: &Client, char_id: i64) -> Result<Value, CallError> {
    advance_training(c, char_id).await?;
    let rows = c
        .db
        .query(
            "SELECT position, type_id, to_level, start_sp, dest_sp, start_time, end_time
             FROM game.skill_queue WHERE character_id = $1 ORDER BY position",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::List(
        rows.iter()
            .map(|r| {
                keyval([
                    ("queuePosition", Value::Int(r.get::<_, i32>(0) as i64)),
                    ("trainingTypeID", Value::Int(r.get::<_, i32>(1) as i64)),
                    ("trainingToLevel", Value::Int(r.get::<_, i32>(2) as i64)),
                    ("trainingStartSP", Value::Int(r.get(3))),
                    ("trainingDestinationSP", Value::Int(r.get(4))),
                    ("trainingStartTime", opt(r.get(5))),
                    ("trainingEndTime", opt(r.get(6))),
                ])
            })
            .collect(),
    ))
}

/// `SaveNewQueue({idx: (typeID, toLevel)}, activate=bool)`: rebuild the queue with timings.
async fn save_queue(c: &Client, char_id: i64, queue: &Value, activate: bool) -> Result<(), CallError> {
    advance_training(c, char_id).await?;
    let training = in_training_sp(c, char_id).await?;
    let mut entries: Vec<(i64, i64, i64)> = match queue {
        Value::Dict(pairs) => pairs
            .iter()
            .filter_map(|(k, v)| {
                let t = v.items()?;
                Some((k.as_int()?, t.first()?.as_int()?, t.get(1)?.as_int()?))
            })
            .collect(),
        _ => vec![],
    };
    entries.sort();
    // Bank SP earned on the skill that was training, so a reordered queue resumes from it.
    if let Some((t, sp)) = training {
        c.db
            .execute(
                "UPDATE game.skills SET skill_points = GREATEST(skill_points, $3) WHERE character_id = $1 AND type_id = $2",
                &[&char_id, &t, &sp],
            )
            .await
            .map_err(db_err)?;
    }
    let attrs = character_attributes(c, char_id).await?;
    let current: HashMap<i32, (i32, i64)> = c
        .db
        .query("SELECT type_id, level, skill_points FROM game.skills WHERE character_id = $1", &[&char_id])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2))))
        .collect();
    c.db.execute("DELETE FROM game.skill_queue WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?;
    let mut t = filetime_now();
    for (pos, (_, type_id, to_level)) in entries.into_iter().enumerate() {
        let st = skill_type(c, type_id as i32).await?;
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
        c.db
            .execute(
                "INSERT INTO game.skill_queue (character_id, position, type_id, to_level, start_sp, dest_sp, start_time, end_time)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&char_id, &(pos as i32), &(type_id as i32), &(to_level as i32), &start_sp, &dest_sp, &start, &end],
            )
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let r = match (call.service, call.method) {
        ("skillMgr2", "GetMySkillHandler") => Ok(c.bind(SKILL_HANDLER, Value::tuple([Value::Int(me)]))),
        (SKILL_HANDLER, "GetSkills") | (SKILL_HANDLER, "GetAllSkills") => skills_dict(c, me).await,
        // [(typeID, pointChange)] since the last login
        (SKILL_HANDLER, "GetSkillChangesForISIS") => Ok(Value::list([])),
        (SKILL_HANDLER, "CheckAndSendNotifications") => Ok(Value::None),
        (SKILL_HANDLER, "GetSkillQueueAndFreePoints") => {
            let free: i64 = match c.db.query_one("SELECT free_skill_points FROM game.characters WHERE character_id = $1", &[&me]).await {
                Ok(r) => r.get(0),
                Err(e) => return Some(Err(db_err(e))),
            };
            queue_list(c, me).await.map(|q| Value::tuple([q, Value::Int(free)]))
        }
        (SKILL_HANDLER, "GetSkillQueue") => queue_list(c, me).await,
        (SKILL_HANDLER, "SaveNewQueue") => {
            let activate = call.kwargs.get("activate").or_else(|| call.args.get(1)).map(|v| v.as_int() != Some(0)).unwrap_or(true);
            match save_queue(c, me, call.args.first().unwrap_or(&Value::None), activate).await {
                Ok(()) => {
                    let q = queue_list(c, me).await;
                    if let Ok(q) = &q {
                        c.notify("OnNewSkillQueueSaved", vec![q.clone()]);
                    }
                    Ok(Value::None)
                }
                Err(e) => Err(e),
            }
        }
        (SKILL_HANDLER, "AbortTraining") => match save_queue(c, me, &Value::Dict(vec![]), false).await {
            Ok(()) => Ok(Value::None),
            Err(e) => Err(e),
        },
        (SKILL_HANDLER, "GetFreeSkillPoints") => {
            match c.db.query_one("SELECT free_skill_points FROM game.characters WHERE character_id = $1", &[&me]).await {
                Ok(r) => Ok(Value::Int(r.get(0))),
                Err(e) => Err(db_err(e)),
            }
        }
        (SKILL_HANDLER, "GetSkillPoints") => match c
            .db
            .query_one("SELECT COALESCE(sum(skill_points), 0)::bigint FROM game.skills WHERE character_id = $1", &[&me])
            .await
        {
            Ok(r) => Ok(Value::Int(r.get(0))),
            Err(e) => Err(db_err(e)),
        },
        (SKILL_HANDLER, "GetAttributes") => character_attributes(c, me)
            .await
            .map(|a| Value::Dict(a.into_iter().map(|(k, v)| (Value::Int(k), Value::Float(v))).collect())),
        (SKILL_HANDLER, "GetBoosters") | (SKILL_HANDLER, "GetImplants") => Ok(Value::Dict(vec![])),
        (SKILL_HANDLER, "GetSkillHistory") => Ok(Value::list([])),
        (SKILL_HANDLER, "GetPenaltySkillPoints") | (SKILL_HANDLER, "GetDiminishedSpFromInjectors") => Ok(Value::Int(0)),
        (SKILL_HANDLER, "GetRespecInfo") => Ok(keyval([
            ("freeRespecs", Value::Int(1)),
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
