//! Jump clones (jumpCloneSvc, bound per station; eve/client/script/ui/services/clonejumpsvc.py).
//!
//! A character can keep one jump clone per station (game.jump_clones), as many as
//! Infomorph Psychology and Advanced Infomorph Psychology levels allow. Clone jumping from a
//! station to one of them leaves the current body behind as a clone there and wakes the
//! pilot in a fresh capsule at the destination; it can be done once per 24 hours, an hour
//! less per Infomorph Synchronizing level. Implants aren't modelled, so clones carry none
//! and cost nothing. Ship-borne clone vat bays aren't supported.

use super::{crowset, db_err, notify_error, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{FILETIME_HOUR, FLAG_HANGAR, TYPE_CAPSULE};
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::info;

const SKILL_INFOMORPH_PSYCHOLOGY: i32 = 24242;
const SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY: i32 = 33407;
const SKILL_INFOMORPH_SYNCHRONIZING: i32 = 33399;
/// Existing server policy: base jump clone cooldown before skill reduction, in hours.
const BASE_JUMP_COOLDOWN_HOURS: i64 = 24;
/// Existing server policy: maximum jump clone name characters.
const MAX_CLONE_NAME_LENGTH: usize = 100;

async fn skill(c: &Client, me: i64, skill: i32) -> Result<i64, CallError> {
    Ok(c.db
        .query_opt("SELECT level FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&me, &skill])
        .await
        .map_err(db_err)?
        .map(|r| r.get::<_, i32>(0) as i64)
        .unwrap_or(0))
}

/// The character's clones as `(jumpCloneID, ownerID, locationID, typeID, cloneName)` rows.
async fn clone_rows(c: &Client, me: i64) -> Result<Value, CallError> {
    let header = DbRowDescriptor::new([
        ("jumpCloneID", DbType::I4),
        ("ownerID", DbType::I4),
        ("locationID", DbType::I8),
        ("typeID", DbType::I4),
        ("cloneName", DbType::WStr),
    ]);
    let rows = c.db.query("SELECT clone_id, location_id, name FROM game.jump_clones WHERE owner_id = $1 ORDER BY clone_id", &[&me]).await.map_err(db_err)?;
    let lines = rows
        .iter()
        .map(|r| header.row(&[Value::Int(r.get(0)), Value::Int(me), Value::Int(r.get(1)), Value::Int(TYPE_CAPSULE as i64), Value::unicode(r.get::<_, String>(2))]))
        .collect();
    Ok(crowset(&header, lines))
}

/// `GetCloneState` -> KeyVal(clones, implants, timeLastJump).
async fn clone_state(c: &Client, me: i64) -> CallResult {
    let last: Option<i64> =
        c.db.query_one("SELECT last_clone_jump FROM game.characters WHERE character_id = $1", &[&me]).await.map_err(db_err)?.get(0);
    Ok(super::keyval([
        ("clones", clone_rows(c, me).await?),
        ("implants", crowset(&DbRowDescriptor::new([("jumpCloneID", DbType::I4), ("typeID", DbType::I4)]), vec![])),
        ("timeLastJump", last.map(Value::Int).unwrap_or(Value::None)),
    ]))
}

async fn install(c: &Client, me: i64) -> CallResult {
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error("你必须停靠在空间站内才能安装远距克隆。"));
    };
    let max = skill(c, me, SKILL_INFOMORPH_PSYCHOLOGY).await? + skill(c, me, SKILL_ADVANCED_INFOMORPH_PSYCHOLOGY).await?;
    let rows = c.db.query("SELECT location_id FROM game.jump_clones WHERE owner_id = $1", &[&me]).await.map_err(db_err)?;
    if rows.iter().any(|r| r.get::<_, i64>(0) == station) {
        return Err(notify_error("你在这个空间站已经有一个远距克隆。"));
    }
    if rows.len() as i64 >= max {
        return Err(notify_error(&format!("你的意识形态心理学技能允许 {max} 个远距克隆。")));
    }
    c.db.execute("INSERT INTO game.jump_clones (owner_id, location_id) VALUES ($1, $2)", &[&me, &station]).await.map_err(db_err)?;
    info!("clones: {me} installed a jump clone at {station}");
    c.world.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
    Ok(Value::None)
}

/// `CloneJump(destLocationID, cloneID, cost, confirmed)`.
async fn jump(c: &mut Client, me: i64, dest: i64, clone_id: i64) -> CallResult {
    let Some(station) = c.session.int("stationid") else {
        return Err(CallError::User { msg: "NotAtStation".into(), dict: vec![] });
    };
    let last: Option<i64> =
        c.db.query_one("SELECT last_clone_jump FROM game.characters WHERE character_id = $1", &[&me]).await.map_err(db_err)?.get(0);
    let cooldown = (BASE_JUMP_COOLDOWN_HOURS - skill(c, me, SKILL_INFOMORPH_SYNCHRONIZING).await?) * FILETIME_HOUR;
    let now = filetime_now();
    if let Some(ready) = last.map(|l| l + cooldown).filter(|r| *r > now) {
        let hours = (ready - now) as f64 / FILETIME_HOUR as f64;
        return Err(notify_error(&format!("你需要再等 {hours:.1} 小时才能再次进行远距克隆跳跃。")));
    }
    let target = c
        .db
        .query_opt(
            "SELECT s.station_id::bigint, s.solar_system_id::bigint, s.constellation_id::bigint, s.region_id::bigint
             FROM game.jump_clones j JOIN sde.sta_stations s ON s.station_id::bigint = j.location_id
             WHERE j.clone_id = $1 AND j.owner_id = $2 AND j.location_id = $3",
            &[&clone_id, &me, &dest],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| notify_error("该远距克隆已不在原处。"))?;
    let (to, system, constellation, region): (i64, i64, i64, i64) = (target.get(0), target.get(1), target.get(2), target.get(3));
    if to == station {
        return Err(notify_error("你已经在该空间站中。"));
    }
    // The body left behind becomes a clone here; the destination clone is used up.
    c.db.execute("DELETE FROM game.jump_clones WHERE clone_id = $1", &[&clone_id]).await.map_err(db_err)?;
    c.db
        .execute("INSERT INTO game.jump_clones (owner_id, location_id) SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM game.jump_clones WHERE owner_id = $1 AND location_id = $2)", &[&me, &station])
        .await
        .map_err(db_err)?;
    // The pod the pilot sat in stays empty; they wake in a new one.
    if let Some(ship) = c.session.int("shipid") {
        c.db.execute("DELETE FROM game.items WHERE item_id = $1 AND type_id = $2", &[&ship, &TYPE_CAPSULE]).await.map_err(db_err)?;
    }
    let capsule: i64 = c
        .db
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
            &[&TYPE_CAPSULE, &me, &to, &FLAG_HANGAR],
        )
        .await
        .map_err(db_err)?
        .get(0);
    c.db
        .execute(
            "UPDATE game.characters SET ship_id = $2, station_id = $3, solar_system_id = $4, constellation_id = $5, region_id = $6, last_clone_jump = $7
             WHERE character_id = $1",
            &[&me, &capsule, &to, &system, &constellation, &region, &now],
        )
        .await
        .map_err(db_err)?;
    info!("clones: {me} clone jumped from {station} to {to}");
    c.change_session(vec![
        ("stationid", Value::Int(to)),
        ("solarsystemid2", Value::Int(system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
        ("shipid", Value::Int(capsule)),
    ]);
    c.world.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
    Ok(Value::None)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "jumpCloneSvc" {
        return None;
    }
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "GetCloneState" => clone_state(c, me).await,
        "GetShipCloneState" | "GetStationCloneState" => {
            let header = DbRowDescriptor::new([("jumpCloneID", DbType::I4), ("ownerID", DbType::I4), ("locationID", DbType::I8), ("typeID", DbType::I4), ("cloneName", DbType::WStr)]);
            Ok(crowset(&header, vec![]))
        }
        "GetNumClonesInPilotsStructure" => Ok(Value::Int(0)),
        "ValidateInstallJumpClone" => Ok(Value::list([])),
        "GetPriceForClone" => Ok(Value::Float(0.0)),
        "InstallCloneInStation" => install(c, me).await,
        "InstallCloneInStructure" => Err(notify_error("暂不支持建筑。")),
        "DestroyInstalledClone" => match c.db.execute("DELETE FROM game.jump_clones WHERE clone_id = $1 AND owner_id = $2", &[&int(0), &me]).await {
            Ok(_) => {
                c.world.notify_pilot(me, "OnJumpCloneCacheInvalidated", vec![]);
                Ok(Value::None)
            }
            Err(e) => Err(db_err(e)),
        },
        // SetJumpCloneName(cloneID, name)
        "SetJumpCloneName" => {
            let name = value_text(call.args.get(1), MAX_CLONE_NAME_LENGTH);
            c.db
                .execute("UPDATE game.jump_clones SET name = $3 WHERE clone_id = $1 AND owner_id = $2", &[&int(0), &me, &name])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        "CloneJump" => jump(c, me, int(0), int(1)).await,
        "ResetLastCloneJumpTime" => Ok(Value::None),
        _ => return None,
    })
}
