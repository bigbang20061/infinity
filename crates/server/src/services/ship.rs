//! Undocking, flying and docking: the `ship` service (bound per location) and
//! the `beyonce` remote ballpark (bound per solar system).
//! Movement runs in per-system actors; arrival cloak expiry is handled by the actor tick.
//! Uses ship attributes and celestial data from the SDE and persists locations in game tables.
//! Mirrors the client ship service and remote ballpark; NPC workers and actor timers are out of scope.

use super::{Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::services::charsel::FLAG_HANGAR;
use crate::space::{self, event, now_ms, now_tick, warp_duration, Ball, Warp, V3, CLOAK_RESTORE, MODE_FOLLOW, MODE_GOTO, MODE_ORBIT, MODE_STOP, MODE_WARP};
use marshal::Value;
use tracing::{info, warn};

const ATTR_MASS: i32 = 4;
const ATTR_MAX_VELOCITY: i32 = 37;
const ATTR_AGILITY: i32 = 70;
const ATTR_WARP_SPEED_MULTIPLIER: i32 = 600;
const ATTR_BASE_WARP_SPEED: i32 = 1281;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

fn f(v: Option<&Value>) -> f64 {
    v.and_then(|v| v.as_float()).unwrap_or(0.0)
}

/// Build a free ship ball from the ship's type attributes.
pub(super) async fn ship_ball(c: &Client, ship_id: i64, pos: [f64; 3]) -> Result<Ball, CallError> {
    let r = c
        .db
        .client
        .query_one(
            "SELECT i.type_id, t.radius,
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $2),
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $3),
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $4),
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $5),
                    (SELECT value FROM sde.type_attributes WHERE type_id = i.type_id AND attribute_id = $6),
                    t.mass
             FROM game.items i JOIN sde.inv_types t USING (type_id) WHERE i.item_id = $1",
            &[&ship_id, &ATTR_MASS, &ATTR_MAX_VELOCITY, &ATTR_AGILITY, &ATTR_WARP_SPEED_MULTIPLIER, &ATTR_BASE_WARP_SPEED],
        )
        .await
        .map_err(db_err)?;
    let mass = r.get::<_, Option<f64>>(2).or(r.get(7)).unwrap_or(1_000_000.0);
    let warp_mult = r.get::<_, Option<f64>>(5).unwrap_or(1.0);
    let base_warp = r.get::<_, Option<f64>>(6).unwrap_or(3.0);
    let char_id = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    // Speed, mass and agility as fitted (skills, modules), when the ship is the pilot's.
    let fit = super::fit::Fit::load(&c.db, char_id, ship_id).await?;
    let fitted = |attr: i32, base: f64| match fit.value(super::fit::Key::Ship, attr) {
        v if v > 0.0 => v,
        _ => base,
    };
    let mass = fitted(ATTR_MASS, mass);
    let security = super::crimewatch::security_status(&c.db, char_id).await?;
    Ok(Ball {
        id: ship_id,
        type_id: r.get(0),
        owner_id: char_id,
        corp_id: corp,
        alliance_id: c.session.int("allianceid").unwrap_or(-1),
        char_id: Some(char_id),
        mode: MODE_STOP,
        radius: r.get::<_, Option<f64>>(1).unwrap_or(50.0) as f32,
        mass,
        max_vel: fitted(ATTR_MAX_VELOCITY, r.get::<_, Option<f64>>(3).unwrap_or(200.0)) as f32,
        agility: fitted(ATTR_AGILITY, r.get::<_, Option<f64>>(4).unwrap_or(3.0)) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        warp: None,
        warp_factor: ((warp_mult * base_warp * 1000.0).round() as i64).max(500),
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("securityStatus", Value::Float(security))],
    })
}

async fn undock(c: &mut Client) -> CallResult {
    let (Some(station), Some(system), Some(ship)) = (c.session.int("stationid"), c.session.int("solarsystemid2"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: "CantUndockNotInStation".into(), dict: vec![] });
    };
    let st = c
        .db
        .client
        .query_one("SELECT x, y, z, COALESCE(radius, 5000)::float8 FROM sde.map_celestials WHERE item_id = $1", &[&station])
        .await
        .map_err(db_err)?;
    let spos: [f64; 3] = [st.get(0), st.get(1), st.get(2)];
    // Leave along a fixed per-station heading.
    let a = (station % 360) as f64 * std::f64::consts::PI / 180.0;
    let dir = [a.cos(), 0.0, a.sin()];
    let dist = st.get::<_, f64>(3) + 2500.0;
    let pos = [spos[0] + dir[0] * dist, spos[1], spos[2] + dir[2] * dist];
    let mut ball = ship_ball(c, ship, pos).await?;
    ball.vel = [dir[0] * ball.max_vel as f64, 0.0, dir[2] * ball.max_vel as f64];
    c.db
        .client
        .execute(
            "UPDATE game.items SET location_id = $2, flag_id = 0, x = $3, y = $4, z = $5 WHERE item_id = $1",
            &[&ship, &system, &pos[0], &pos[1], &pos[2]],
        )
        .await
        .map_err(db_err)?;
    c.db
        .client
        .execute("UPDATE game.characters SET station_id = NULL, solar_system_id = $2 WHERE character_id = $1", &[&c.session.int("charid").unwrap_or(0), &system])
        .await
        .map_err(db_err)?;
    c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    info!(user = c.login.user_id, "ship {ship} undocked from {station} into {system}");
    c.change_session(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    Ok(Value::None)
}

/// Refuse docking and gate jumps while the pilot's weapons timer runs (crimewatch).
fn check_weapons_timer(c: &Client, what: &str) -> Result<(), CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    match super::crimewatch::weapons_timer(char_id) {
        Some(until) => {
            let secs = ((until - crate::client::filetime_now()) / 10_000_000).max(1);
            Err(CallError::User {
                msg: "CustomNotify".into(),
                dict: vec![("notify".into(), Value::unicode(format!("武器计时器仍在生效，{secs} 秒内无法{what}。")))],
            })
        }
        None => Ok(()),
    }
}

async fn dock(c: &mut Client, station: i64) -> CallResult {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: "DockingRequestDenied".into(), dict: vec![] });
    };
    check_weapons_timer(c, "停靠")?;
    super::combat::reset(c);
    // Station services patch the ship up.
    super::combat::repair(ship);
    c.world.remove_ball(system, ship).await;
    c.world.unobserve(c.login.client_id);
    c.db
        .client
        .execute("UPDATE game.items SET location_id = $2, flag_id = $3, x = NULL, y = NULL, z = NULL WHERE item_id = $1", &[&ship, &station, &FLAG_HANGAR])
        .await
        .map_err(db_err)?;
    c.db
        .client
        .execute("UPDATE game.characters SET station_id = $2, solar_system_id = $3 WHERE character_id = $1", &[&c.session.int("charid").unwrap_or(0), &station, &system])
        .await
        .map_err(db_err)?;
    info!(user = c.login.user_id, "ship {ship} docked at {station}");
    c.change_session(vec![("solarsystemid", Value::None), ("stationid", Value::Int(station))]);
    Ok(Value::None)
}

/// Jump range: ship and gate surfaces within this distance (m).
const JUMP_RANGE: f64 = 2500.0;

/// `effects.*` special effect on `ship` aimed at `target` (spaceMgr/fxSequencer.OnSpecialFX).
/// Travel effects carry `graphicInfo = (destinationSystemID,)`: subwaySvc.StartJump preloads
/// that system's scene for the jump tunnel; `other` is a wormhole jump's destination class.
fn special_fx(tick: i64, ship: i64, target: Option<i64>, other: Option<i64>, guid: &str, graphic_info: Option<Value>) -> Value {
    let target = target.map(Value::Int).unwrap_or(Value::None);
    // (shipID, moduleID, moduleTypeID, targetID, otherTypeID, guid, isOffensive, start, active,
    //  duration, repeat, startTime, timeFromStart, graphicInfo)
    let args = vec![
        Value::Int(ship),
        Value::None,
        Value::None,
        target,
        other.map(Value::Int).unwrap_or(Value::None),
        Value::str(guid),
        Value::Bool(false),
        Value::Int(1),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(0),
        Value::None,
        Value::Int(0),
        graphic_info.unwrap_or(Value::None),
    ];
    event(tick, "OnSpecialFX", args)
}

/// Stargate jump delay (ballparkCommon/jumpdelay.py: appConst.invulnerabilityJumping, ms):
/// the gate animation plays this long before the session moves to the next system.
const JUMP_DELAY_MS: u64 = 4000;
/// The arrival cloak lasts this long unless the pilot moves first (s).
const JUMP_CLOAK_SECONDS: i64 = 60;
const SEC: i64 = 10_000_000;
const UE_LOCID: i64 = 3;

/// CmdStargateJump(fromStargateID, toStargateID, shipID): leave through the gate and
/// arrive, cloaked, next to its destination gate in the neighbouring system.
async fn stargate_jump(c: &mut Client, from_gate: i64, to_gate: i64) -> CallResult {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: "JumpGateHasNoDestination".into(), dict: vec![] });
    };
    check_weapons_timer(c, "跳跃")?;
    let dest = c
        .db
        .client
        .query_opt(
            "SELECT g.dest_solar_system_id::bigint, s.constellation_id::bigint, s.region_id::bigint, c.x, c.y, c.z,
                    COALESCE(c.radius, t.radius, 5000)::float8
             FROM sde.map_stargates g
             JOIN sde.map_solar_systems s ON s.solar_system_id = g.dest_solar_system_id
             JOIN sde.map_celestials c ON c.item_id = g.dest_stargate_id
             JOIN sde.inv_types t ON t.type_id = c.type_id
             WHERE g.stargate_id = $1 AND g.dest_stargate_id = $2 AND g.solar_system_id = $3",
            &[&(from_gate as i32), &(to_gate as i32), &(system as i32)],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: "JumpGateHasNoDestination".into(), dict: vec![] })?;
    let dest_system: i64 = dest.get(0);

    // In range of the gate?
    let gap = c.world.ball(system, ship).await.zip(c.world.ball(system, from_gate).await).map(|(b, gate)| {
        ((b.pos[0] - gate.pos[0]).powi(2) + (b.pos[1] - gate.pos[1]).powi(2) + (b.pos[2] - gate.pos[2]).powi(2)).sqrt()
            - gate.radius as f64
            - b.radius as f64
    });
    if gap.is_none_or(|g| g > JUMP_RANGE) {
        return Err(CallError::User { msg: "NotCloseEnoughToJump".into(), dict: vec![("maxdist".into(), Value::Int(JUMP_RANGE as i64))] });
    }
    // Arrive a few kilometres off the destination gate, on a heading fixed per gate pair.
    let gate_pos: [f64; 3] = [dest.get(3), dest.get(4), dest.get(5)];
    let a = ((from_gate ^ to_gate) % 360) as f64 * std::f64::consts::PI / 180.0;
    let dist = dest.get::<_, f64>(6) + 5000.0;
    let pos = [gate_pos[0] + a.cos() * dist, gate_pos[1], gate_pos[2] + a.sin() * dist];
    let out = Jump { through: from_gate, guid: "effects.JumpOut", other_type: None, dest_system, pos };
    jump_out_and_arrive(c, system, ship, char_id, out).await?;
    info!(user = c.login.user_id, "ship {ship} jumped {from_gate} ({system}) -> {to_gate} ({dest_system})");
    Ok(Value::None)
}

/// A jump out of `system` through `through` (a gate, a wormhole) to `pos` in `dest_system`.
pub(super) struct Jump {
    pub through: i64,
    pub guid: &'static str,
    pub other_type: Option<i64>,
    pub dest_system: i64,
    pub pos: V3,
}

/// Leave through a gate or wormhole: play the jump-out effect where everyone sees it, wait
/// out the animation, then arrive cloaked in the destination system and move the session.
pub(super) async fn jump_out_and_arrive(c: &mut Client, system: i64, ship: i64, char_id: i64, jump: Jump) -> Result<(), CallError> {
    let Jump { through, guid, other_type, dest_system, pos } = jump;
    let dest = c
        .db
        .client
        .query_one("SELECT constellation_id::bigint, region_id::bigint FROM sde.map_solar_systems WHERE solar_system_id = $1", &[&(dest_system as i32)])
        .await
        .map_err(db_err)?;
    let (constellation, region): (i64, i64) = (dest.get(0), dest.get(1));
    c.world.system(dest_system).await.map_err(CallError::Internal)?;
    // Room on the other side, or a place in its jump queue.
    if let Err(position) = c.world.request_entry(dest_system, char_id).await {
        info!(user = c.login.user_id, "jump into {dest_system} queued at {position}");
        let msg = if position == 0 { "SystemCheck_JumpFailed_Loading" } else { "SystemCheck_JumpFailed_Queued" };
        return Err(CallError::User {
            msg: msg.into(),
            dict: vec![("system".into(), Value::tuple([Value::Int(UE_LOCID), Value::Int(dest_system)])), ("position".into(), Value::Int(position.max(1)))],
        });
    }

    // Jump out where everyone can see it, and let the gate animation run.
    c.world.uncloak(system, ship).await;
    c.world.command(system, ship, move |b, t, _| {
        b.mode = MODE_STOP;
        b.speed_fraction = 0.0;
        b.vel = [0.0; 3];
        vec![
            event(t, "Stop", vec![Value::Int(b.id)]),
            special_fx(t, b.id, Some(through), other_type, guid, Some(Value::tuple([Value::Int(dest_system)]))),
        ]
    });
    c.pump().await.map_err(CallError::Internal)?;
    tokio::time::sleep(std::time::Duration::from_millis(JUMP_DELAY_MS)).await;

    super::combat::reset(c);
    c.world.remove_ball(system, ship).await;
    c.world.unobserve(c.login.client_id);
    c.db.client
        .execute("UPDATE game.items SET location_id = $2, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship, &dest_system, &pos[0], &pos[1], &pos[2]])
        .await
        .map_err(db_err)?;
    c.db.client
        .execute(
            "UPDATE game.characters SET solar_system_id = $2, constellation_id = $3, region_id = $4 WHERE character_id = $1",
            &[&char_id, &dest_system, &constellation, &region],
        )
        .await
        .map_err(db_err)?;
    let cloak_until = crate::client::filetime_now() + JUMP_CLOAK_SECONDS * SEC;
    let mut ball = ship_ball(c, ship, pos).await?;
    ball.cloak = CLOAK_RESTORE;
    ball.cloak_until = cloak_until;
    c.world.add_ship(dest_system, ball).await.map_err(CallError::Internal)?;
    // The client rebinds its remote ballpark for the new system, which sends SetState.
    c.change_session(vec![
        ("solarsystemid", Value::Int(dest_system)),
        ("solarsystemid2", Value::Int(dest_system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
    ]);
    // Arrival cloak: crimewatch timer now, lapse after JUMP_CLOAK_SECONDS unless broken.
    c.notify("OnJumpCloakUpdated", vec![Value::Int(ship), Value::Int(cloak_until), Value::Int(JUMP_CLOAK_SECONDS * SEC)]);
    // The client leaves the old Local itself; the new one arrives as an invite, sent once
    // the session change has had time to land.
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    // effects.JumpOutWormhole hides the jumping ship with a repeating client-side
    // CloakRegardless effect; it ends only when told to, once the pilot sees the new system.
    if guid == "effects.JumpOutWormhole" {
        let world = c.world.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            world.broadcast(dest_system, ship, vec![special_fx_stop(now_tick(), ship, "effects.CloakRegardless")]);
        });
    }
    Ok(())
}

/// Stop a repeating special effect on `ship` (OnSpecialFX with start = 0).
fn special_fx_stop(tick: i64, ship: i64, guid: &str) -> Value {
    let args = vec![
        Value::Int(ship),
        Value::None,
        Value::None,
        Value::None,
        Value::None,
        Value::str(guid),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(0),
        Value::None,
        Value::Int(0),
        Value::None,
    ];
    event(tick, "OnSpecialFX", args)
}

/// GM teleport (/tr): put the ship at `pos` in `dest_system` at once. Within the system
/// the client gets a fresh ballpark; elsewhere the session moves as on a jump.
pub(super) async fn teleport(c: &mut Client, dest_system: i64, pos: V3) -> Result<(), CallError> {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), Value::unicode("你必须在太空中。"))] });
    };
    c.world.system(dest_system).await.map_err(CallError::Internal)?;
    super::combat::reset(c);
    c.world.remove_ball(system, ship).await;
    c.db.client
        .execute("UPDATE game.items SET location_id = $2, x = $3, y = $4, z = $5 WHERE item_id = $1", &[&ship, &dest_system, &pos[0], &pos[1], &pos[2]])
        .await
        .map_err(db_err)?;
    let ball = ship_ball(c, ship, pos).await?;
    if dest_system == system {
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
        return send_state(c).await;
    }
    let dest = c
        .db
        .client
        .query_one("SELECT constellation_id::bigint, region_id::bigint FROM sde.map_solar_systems WHERE solar_system_id = $1", &[&(dest_system as i32)])
        .await
        .map_err(db_err)?;
    let (constellation, region): (i64, i64) = (dest.get(0), dest.get(1));
    c.world.unobserve(c.login.client_id);
    c.db.client
        .execute(
            "UPDATE game.characters SET solar_system_id = $2, constellation_id = $3, region_id = $4 WHERE character_id = $1",
            &[&char_id, &dest_system, &constellation, &region],
        )
        .await
        .map_err(db_err)?;
    c.world.add_ship(dest_system, ball).await.map_err(CallError::Internal)?;
    c.change_session(vec![
        ("solarsystemid", Value::Int(dest_system)),
        ("solarsystemid2", Value::Int(dest_system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
    ]);
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    info!(user = c.login.user_id, "ship {ship} teleported {system} -> {dest_system}");
    Ok(())
}

const TYPE_CAPSULE: i32 = 670;

/// The pilot's ship was destroyed in space (combat::damage_player): its fittings and
/// cargo go with it and the pilot ejects in a capsule where it died.
pub async fn lose_ship(c: &mut Client, ship: i64, pos: [f64; 3]) -> Result<(), CallError> {
    let (Some(char_id), Some(system)) = (c.session.int("charid"), c.session.int("solarsystemid")) else { return Ok(()) };
    if c.session.int("shipid") != Some(ship) {
        return Ok(());
    }
    super::combat::reset(c);
    super::combat::forget_ship(ship);
    let lost_type: Option<i32> = c.db.client.query_opt("SELECT type_id FROM game.items WHERE item_id = $1", &[&ship]).await.map_err(db_err)?.map(|r| r.get(0));
    // The killmail lists what went down with it: fittings, cargo and loaded charges.
    let mut lost: super::crimewatch::Lost = c
        .db
        .client
        .query("SELECT type_id, flag_id, singleton, quantity FROM game.items WHERE location_id = $1", &[&ship])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| {
            let singleton: bool = r.get(2);
            (r.get(0), r.get(1), singleton, if singleton { 1 } else { r.get::<_, i32>(3) as i64 })
        })
        .collect();
    for r in c.db.client.query("SELECT type_id, flag_id, quantity FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.map_err(db_err)? {
        lost.push((r.get(0), r.get(1), false, r.get::<_, i32>(2) as i64));
    }
    if let Some(ship_type) = lost_type {
        let corp = c.session.int("corpid").unwrap_or(0);
        if let Err(CallError::Internal(e)) = super::crimewatch::ship_lost(&c.db, &c.world, system, char_id, corp, ship, ship_type, &lost).await {
            warn!(user = c.login.user_id, "killmail for {ship}: {e:#}");
        }
        if let Err(CallError::Internal(e)) = super::insurance::ship_destroyed(&c.db, &c.world, ship).await {
            warn!(user = c.login.user_id, "insurance payout for {ship}: {e:#}");
        }
    }
    if lost_type == Some(TYPE_CAPSULE) {
        return wake_in_clone(c, char_id, ship).await;
    }
    for sql in [
        "DELETE FROM game.items WHERE location_id = $1",
        "DELETE FROM game.module_charges WHERE ship_id = $1",
        "DELETE FROM game.weapon_banks WHERE ship_id = $1",
        "DELETE FROM game.items WHERE item_id = $1",
    ] {
        c.db.client.execute(sql, &[&ship]).await.map_err(db_err)?;
    }
    let capsule: i64 = c
        .db
        .client
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z)
             VALUES ($1, $2, $3, 0, 1, true, $4, $5, $6) RETURNING item_id",
            &[&TYPE_CAPSULE, &char_id, &system, &pos[0], &pos[1], &pos[2]],
        )
        .await
        .map_err(db_err)?
        .get(0);
    c.db.client.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&char_id, &capsule]).await.map_err(db_err)?;
    let ball = ship_ball(c, capsule, pos).await?;
    c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    info!(user = c.login.user_id, "ship {ship} lost; {char_id} ejected in capsule {capsule}");
    c.change_session(vec![("shipid", Value::Int(capsule))]);
    send_state(c).await
}

/// Podded: the pilot wakes up in a new clone at its home station, in a fresh capsule.
async fn wake_in_clone(c: &mut Client, char_id: i64, pod: i64) -> Result<(), CallError> {
    c.db.client.execute("DELETE FROM game.items WHERE item_id = $1", &[&pod]).await.map_err(db_err)?;
    let home = c
        .db
        .client
        .query_one(
            "SELECT s.station_id::bigint, s.solar_system_id::bigint, s.constellation_id::bigint, s.region_id::bigint
             FROM game.characters ch JOIN sde.sta_stations s ON s.station_id = COALESCE(ch.home_station_id, ch.station_id)
             WHERE ch.character_id = $1",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    let (station, system, constellation, region): (i64, i64, i64, i64) = (home.get(0), home.get(1), home.get(2), home.get(3));
    let capsule: i64 = c
        .db
        .client
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
            &[&TYPE_CAPSULE, &char_id, &station, &FLAG_HANGAR],
        )
        .await
        .map_err(db_err)?
        .get(0);
    c.db.client
        .execute(
            "UPDATE game.characters SET ship_id = $2, station_id = $3, solar_system_id = $4, constellation_id = $5, region_id = $6 WHERE character_id = $1",
            &[&char_id, &capsule, &station, &system, &constellation, &region],
        )
        .await
        .map_err(db_err)?;
    c.world.unobserve(c.login.client_id);
    info!(user = c.login.user_id, "{char_id} podded; new clone at {station}");
    c.change_session(vec![
        ("solarsystemid", Value::None),
        ("stationid", Value::Int(station)),
        ("solarsystemid2", Value::Int(system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
        ("shipid", Value::Int(capsule)),
    ]);
    Ok(())
}

/// Push the full destiny state for the client's current system (on beyonce bind / UpdateStateRequest).
pub async fn send_state(c: &mut Client) -> Result<(), CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(()) };
    let set_state = c.world.observe(system, c.login.client_id, ship, c.destiny_tx.clone()).await.map_err(CallError::Internal)?;
    c.notify("DoDestinyUpdate", vec![Value::list([set_state]), Value::Bool(false)]);
    Ok(())
}

/// Steer the ship. Any movement but stopping drops a jump cloak.
fn movement(c: &mut Client, f: impl FnOnce(&mut Ball, i64, &space::System) -> Vec<Value> + Send + 'static) -> CallResult {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(Value::None) };
    let pilot = c.session.int("charid");
    c.world.post(system, move |s| {
        if s.uncloak(ship) {
            if let Some(pilot) = pilot {
                s.notify_pilot(pilot, "OnJumpCloakCancelled", vec![Value::Int(ship)]);
            }
        }
        s.command(ship, f);
    });
    Ok(Value::None)
}

/// Warp the session's ship to `point`. With `object` (position, radius) it lands
/// `min_range` (at least 2.5 km) off that object's surface on the near side; without,
/// at the point itself (bookmarks, mission sites).
pub(super) fn warp_to(c: &mut Client, object: Option<(V3, f64)>, point: V3, min_range: f64) -> CallResult {
    movement(c, move |b, t, _| {
        let to = match object {
            Some((pos, radius)) => {
                let away = [b.pos[0] - pos[0], b.pos[1] - pos[1], b.pos[2] - pos[2]];
                let l = (away[0] * away[0] + away[1] * away[1] + away[2] * away[2]).sqrt().max(1.0);
                let stand_off = radius + b.radius as f64 + min_range.max(2500.0);
                [pos[0] + away[0] / l * stand_off, pos[1] + away[1] / l * stand_off, pos[2] + away[2] / l * stand_off]
            }
            None => point,
        };
        let distance = ((to[0] - b.pos[0]).powi(2) + (to[1] - b.pos[1]).powi(2) + (to[2] - b.pos[2]).powi(2)).sqrt();
        let tau = (b.mass * b.agility as f64 / 1e6).max(0.1);
        let align = -tau * 0.25f64.ln();
        b.mode = MODE_WARP;
        b.goto = to;
        b.speed_fraction = 1.0;
        b.warp = Some(Warp { from: b.pos, to, start: b.last_ms, duration: (align + warp_duration(b.warp_factor as f64, distance)) * 1000.0, factor: b.warp_factor, min_range });
        // Destiny only enters warp once aligned at 75% of max velocity, and thrust scales
        // with the speed fraction (Ball::IsAlignedForWarp, GotoThrust): warp at full throttle,
        // or a stopped ship sits aligning until the 180 s MAX_ALIGN_TICKS fallback.
        // effects.Warping drives the tunnel, camera and audio (and WarpOut for onlookers);
        // it follows the ball through align and warp and the client stops it on exit.
        vec![
            event(t, "WarpTo", vec![Value::Int(b.id), Value::Float(to[0]), Value::Float(to[1]), Value::Float(to[2]), Value::Float(min_range), Value::Int(b.warp_factor)]),
            event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]),
            special_fx(t, b.id, None, None, "effects.Warping", None),
        ]
    })
}

/// Ensure a stationary ship starts moving when given a heading (client sends 1.0 separately too).
fn ensure_moving(b: &mut Ball, tick: i64, ev: &mut Vec<Value>) {
    if b.speed_fraction <= 0.0 {
        b.speed_fraction = 1.0;
        ev.push(event(tick, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]));
    }
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let a = call.args;
    Some(match (call.service, call.method) {
        ("ship", "Undock") => undock(c).await,
        ("beyonce", "UpdateStateRequest") => send_state(c).await.map(|_| Value::None),
        ("beyonce", "CmdGotoDirection") => {
            let dir = [f(a.first()), f(a.get(1)), f(a.get(2))];
            movement(c, move |b, t, _| {
                b.mode = MODE_GOTO;
                b.goto = dir;
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdAlignTo") => {
            let target = call.kwargs.get("dstID").and_then(|v| v.as_int()).or_else(|| a.first().and_then(|v| v.as_int()));
            movement(c, move |b, t, others| {
                let Some(tp) = target.and_then(|id| others.ball(id)).map(|o| o.pos) else { return vec![] };
                let dir = [tp[0] - b.pos[0], tp[1] - b.pos[1], tp[2] - b.pos[2]];
                b.mode = MODE_GOTO;
                b.goto = dir;
                let mut ev = vec![event(t, "GotoDirection", vec![Value::Int(b.id), Value::Float(dir[0]), Value::Float(dir[1]), Value::Float(dir[2])])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdStop") => {
            if let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) {
                c.world.command(system, ship, |b, t, _| {
                    b.mode = MODE_STOP;
                    b.speed_fraction = 0.0;
                    vec![event(t, "Stop", vec![Value::Int(b.id)])]
                });
            }
            Ok(Value::None)
        }
        ("beyonce", "CmdSetSpeedFraction") => {
            let frac = f(a.first()).clamp(0.0, 1.0) as f32;
            movement(c, move |b, t, _| {
                b.speed_fraction = frac;
                vec![event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(frac as f64)])]
            })
        }
        ("beyonce", "CmdFollowBall") | ("beyonce", "CmdOrbit") => {
            let target = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let range = f(a.get(1)) as f32;
            let orbit = call.method == "CmdOrbit";
            movement(c, move |b, t, _| {
                b.mode = if orbit { MODE_ORBIT } else { MODE_FOLLOW };
                b.follow_id = target;
                b.follow_range = range;
                let name = if orbit { "Orbit" } else { "FollowBall" };
                let mut ev = vec![event(t, name, vec![Value::Int(b.id), Value::Int(target), Value::Float(range as f64)])];
                ensure_moving(b, t, &mut ev);
                ev
            })
        }
        ("beyonce", "CmdWarpToStuff") if a.first().and_then(|v| v.as_text()).as_deref() == Some("scan") => {
            let target = a.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            super::exploration::warp_to_scan(c, &target, min_range).await
        }
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => {
            let target = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let min_range = call.kwargs.get("minRange").and_then(|v| v.as_float()).unwrap_or(0.0);
            // 'char': a fleet member's ship.
            let by_pilot = a.first().and_then(|v| v.as_text()).as_deref() == Some("char");
            let found = if let Some(sys) = c.session.int("solarsystemid") {
                c.world.with(sys, move |s| {
                    if by_pilot { s.pilot_ball(target) } else { s.ball(target) }
                        .map(|b| (b.id, b.pos, b.radius as f64))
                }).await.flatten()
            } else {
                None
            };
            let Some((id, pos, radius)) = found else {
                return Some(Ok(Value::None));
            };
            let warped = warp_to(c, Some((pos, radius)), pos, min_range);
            // Warping to an asteroid belt brings out its rats.
            if let (Ok(_), false, Some(system)) = (&warped, by_pilot, c.session.int("solarsystemid")) {
                if let Err(e) = super::exploration::belt_rats(c, system, id, pos).await {
                    warn!(user = c.login.user_id, "belt rats at {}: {}", id, super::exploration::err_text(e));
                }
            }
            warped
        }
        ("beyonce", "CmdStargateJump") => {
            let from_gate = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            let to_gate = a.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            stargate_jump(c, from_gate, to_gate).await
        }
        ("beyonce", "CmdDock") | ("beyonce", "CmdTurboDock") => {
            let station = a.first().and_then(|v| v.as_int()).unwrap_or(0);
            dock(c, station).await
        }
        ("beyonce", m) => {
            warn!(user = c.login.user_id, "UNHANDLED {}::{m} args={} kwargs={}", call.service, short(&marshal::Value::Tuple(a.to_vec())), short(call.kwargs));
            Ok(Value::None)
        }
        _ => return None,
    })
}

