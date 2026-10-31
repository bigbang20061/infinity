//! Undocking, flying and docking through the location-bound `ship` service and
//! system-bound `beyonce` remote ballpark. Destiny updates are consumed by
//! `eve/client/script/remote/michelle.py`; gate destinations are consumed by
//! `eve/client/script/parklife/autopilot.py`. Ship attributes and celestial
//! data come from the SDE; locations are persisted in game tables.
//!
//! Movement commands apply at destiny boundaries; jump waits and arrival cloak
//! expiry use the actor tick. Gate range and live warp targets use positions
//! sampled at the system actor's time. GOTO stores an absolute point; direction
//! commands project a point from the current position and send the raw direction.
//! Carbonengine `Ballpark::WarpTo` (`Ballpark.cpp:4102-4113`) falls back to
//! GotoPoint with no WarpFX when the raw destination is strictly below 100 km;
//! exactly 100 km takes the full-warp path. Full warp timing and landing remain
//! approximate, as do follow and orbit; collision and bump physics are absent.
//! Local-chat invites use an outside-space delay for session settlement.
//! Ship loss commits before its independently best-effort killmail and insurance
//! operations. NPC workers are outside this module's scope.

use super::{db_err, Call, CallError, CallResult};
use crate::client::{short, Client};
use crate::consts::{ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_SIGNATURE_RADIUS, FILETIME_SECOND, FLAG_HANGAR, TYPE_CAPSULE};
use crate::db::outbox::{Effect, Outbox};
use crate::dao::{characters::{ship as character_dao, selection}, items::{ship as item_dao, creation}};
use crate::messages;
use crate::space::{self, event, now_ms, seconds_to_ms_f64, warp_duration, Ball, Warp, V3, CLOAK_RESTORE, MODE_FOLLOW, MODE_GOTO, MODE_ORBIT, MODE_STOP, MODE_WARP};
use marshal::Value;
use tracing::{info, warn};

/// Existing gate and station radius fallback in metres.
const DEFAULT_CELESTIAL_RADIUS: f64 = 5000.0;

/// Carbonengine Ballpark::WarpTo: squared minimum warp distance in metres.
const MIN_WARP_DISTANCE_SQUARED: f64 = 1e10;

fn gate_radius(celestial: Option<f64>, type_radius: Option<f64>) -> f64 {
    celestial.or(type_radius).unwrap_or(DEFAULT_CELESTIAL_RADIUS)
}

fn required<T>(value: Option<T>, field: &str) -> Result<T, CallError> {
    value.ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing SDE {field}")))
}

fn ancestry(sde: &crate::sde::Sde, system: i64) -> Result<(i64, i64), CallError> {
    let row = required(sde.map.system(system), "solar system")?;
    Ok((required(row.constellation_id, "constellation ID")?, required(row.region_id, "region ID")?))
}

struct GateRow { dest_solar_system_id: i64, x: f64, y: f64, z: f64, radius: f64 }

fn gate_destination(sde: &crate::sde::Sde, from: i64, to: i64, system: i64) -> Result<Option<GateRow>, CallError> {
    let Some(gate) = sde.map.stargate(from) else { return Ok(None); };
    if gate.solar_system_id != Some(system) || gate.dest_stargate_id != Some(to) { return Ok(None); }
    let Some(dest_system) = gate.dest_solar_system_id else { return Ok(None); };
    if sde.map.system(dest_system).is_none() { return Ok(None); }
    let Some(celestial) = sde.map.celestial(to) else { return Ok(None); };
    let Some(ty) = celestial.type_id.and_then(|id| i32::try_from(id).ok()).and_then(|id| sde.types.get(id)) else { return Ok(None); };
    Ok(Some(GateRow {
        dest_solar_system_id: dest_system,
        x: required(celestial.x, "gate x")?, y: required(celestial.y, "gate y")?, z: required(celestial.z, "gate z")?,
        radius: gate_radius(celestial.radius, ty.radius),
    }))
}

const ATTR_WARP_SPEED_MULTIPLIER: i32 = 600;
const ATTR_BASE_WARP_SPEED: i32 = 1281;

/// Server policy: ship radius in metres when the SDE has no radius.
const DEFAULT_SHIP_RADIUS: f64 = 50.0;

fn f(v: Option<&Value>) -> f64 {
    v.and_then(|v| v.as_float()).unwrap_or(0.0)
}

/// Build a free ship ball from the ship's type attributes.
pub(super) async fn ship_ball(c: &Client, ship_id: i64, pos: [f64; 3]) -> Result<Ball, CallError> {
    let type_id = required(crate::dao::items::inventory::type_id(&c.db, ship_id).await.map_err(db_err)?, "ship item")?;
    let r = required(c.sde.types.get(type_id), "ship type")?;
    let mass = r.attribute(ATTR_MASS).or(r.mass).unwrap_or(1_000_000.0);
    let warp_mult = r.attribute(ATTR_WARP_SPEED_MULTIPLIER).unwrap_or(1.0);
    let base_warp = r.attribute(ATTR_BASE_WARP_SPEED).unwrap_or(3.0);
    let char_id = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    // Movement and signature attributes as fitted (skills, modules), when the ship is the pilot's.
    let fit = super::fit::Fit::load(&c.db, c.sde.clone(), char_id, ship_id).await?;
    let fitted = |attr: i32, base: f64| match fit.value(super::fit::Key::Ship, attr) {
        v if v > 0.0 => v,
        _ => base,
    };
    let mass = fitted(ATTR_MASS, mass);
    let radius = r.radius.unwrap_or(DEFAULT_SHIP_RADIUS) as f32;
    let security = super::crimewatch::security_status(&c.db, char_id).await?;
    Ok(Ball {
        id: ship_id,
        type_id,
        owner_id: char_id,
        corp_id: corp,
        alliance_id: c.session.int("allianceid").unwrap_or(-1),
        char_id: Some(char_id),
        mode: MODE_STOP,
        radius,
        signature_radius: Some(fitted(ATTR_SIGNATURE_RADIUS, radius as f64)),
        mass,
        max_vel: fitted(ATTR_MAX_VELOCITY, r.attribute(ATTR_MAX_VELOCITY).unwrap_or(200.0)) as f32,
        agility: fitted(ATTR_AGILITY, r.attribute(ATTR_AGILITY).unwrap_or(3.0)) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_target: None, segment_warp: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        interactive: true,
        warp: None,
        warp_factor: ((warp_mult * base_warp * 1000.0).round() as i64).max(500),
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("securityStatus", Value::Float(security))],
    })
}

/// Refuse docking and gate jumps while the pilot's weapons timer runs (crimewatch).
fn check_weapons_timer(c: &Client, what: &str) -> Result<(), CallError> {
    let char_id = c.session.int("charid").unwrap_or(0);
    match super::crimewatch::weapons_timer(char_id) {
        Some(until) => {
            let secs = ((until - crate::client::filetime_now()) / 10_000_000).max(1);
            Err(CallError::User {
                msg: messages::MSG_CUSTOM_NOTIFY.into(),
                dict: vec![("notify".into(), Value::unicode(messages::weapons_timer_message(secs, what)))],
            })
        }
        None => Ok(()),
    }
}

async fn dock(c: &mut Client, station: i64) -> CallResult {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: messages::MSG_DOCKING_REQUEST_DENIED.into(), dict: vec![] });
    };
    check_weapons_timer(c, messages::WEAPONS_TIMER_DOCK)?;
    super::combat::reset(c);
    // Station services patch the ship up.
    super::combat::repair(ship);
    c.world.remove_ball(system, ship).await;
    c.world.unobserve(c.login.client_id);
    item_dao::dock(&c.db, ship, station, FLAG_HANGAR).await.map_err(db_err)?;
    character_dao::dock(&c.db, c.session.int("charid").unwrap_or(0), station, system).await.map_err(db_err)?;
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
/// Existing server policy: allow the session change to settle before inviting Local chat.
const LOCAL_CHAT_INVITE_DELAY_MS: u64 = 2000;
/// Existing server timing for stopping the wormhole effects.CloakRegardless effect.
const WORMHOLE_EFFECT_STOP_DELAY_MS: u64 = 3000;
/// The arrival cloak lasts this long unless the pilot moves first (s).
const JUMP_CLOAK_SECONDS: i64 = 60;
const UE_LOCID: i64 = 3;

/// CmdStargateJump(fromStargateID, toStargateID, shipID): leave through the gate and
/// arrive, cloaked, next to its destination gate in the neighbouring system.
async fn stargate_jump(c: &mut Client, from_gate: i64, to_gate: i64) -> CallResult {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Err(CallError::User { msg: messages::MSG_JUMP_GATE_HAS_NO_DESTINATION.into(), dict: vec![] });
    };
    check_weapons_timer(c, messages::WEAPONS_TIMER_JUMP)?;
    let dest = gate_destination(&c.sde, from_gate, to_gate, system)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_JUMP_GATE_HAS_NO_DESTINATION.into(), dict: vec![] })?;
    let dest_system: i64 = dest.dest_solar_system_id;

    // In range of the gate?
    let gap = c.world.with(system, move |s| {
        let (b, gate) = (s.ball(ship)?, s.ball(from_gate)?);
        Some(space::dist(b.position_at(s.now_ms()), gate.position_at(s.now_ms()))
            - gate.radius as f64
            - b.radius as f64)
    }).await.flatten();
    if gap.is_none_or(|g| g > JUMP_RANGE) {
        return Err(CallError::User { msg: messages::MSG_NOT_CLOSE_ENOUGH_TO_JUMP.into(), dict: vec![("maxdist".into(), Value::Int(JUMP_RANGE as i64))] });
    }
    // Arrive a few kilometres off the destination gate, on a heading fixed per gate pair.
    let gate_pos: [f64; 3] = [dest.x, dest.y, dest.z];
    let a = ((from_gate ^ to_gate) % 360) as f64 * std::f64::consts::PI / 180.0;
    let dist = dest.radius + 5000.0;
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
    let (constellation, region) = ancestry(&c.sde, dest_system)?;
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
    if !c.world.after(system, JUMP_DELAY_MS).await {
        return Err(CallError::Internal(anyhow::anyhow!("source system actor stopped during jump wait")));
    }

    super::combat::reset(c);
    c.world.remove_ball(system, ship).await;
    c.world.unobserve(c.login.client_id);
    item_dao::position(&c.db, ship, dest_system, pos).await.map_err(db_err)?;
    character_dao::location(&c.db, char_id, dest_system, constellation, region).await.map_err(db_err)?;
    let cloak_until = crate::client::filetime_now() + JUMP_CLOAK_SECONDS * FILETIME_SECOND;
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
    c.notify("OnJumpCloakUpdated", vec![Value::Int(ship), Value::Int(cloak_until), Value::Int(JUMP_CLOAK_SECONDS * FILETIME_SECOND)]);
    // The client leaves the old Local itself; the new one arrives as an invite, sent once
    // the session change has had time to land.
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(LOCAL_CHAT_INVITE_DELAY_MS)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    // effects.JumpOutWormhole hides the jumping ship with a repeating client-side
    // CloakRegardless effect; it ends only when told to, once the pilot sees the new system.
    if guid == "effects.JumpOutWormhole" {
        c.world.schedule(dest_system, WORMHOLE_EFFECT_STOP_DELAY_MS, move |s| {
            s.broadcast(
                ship,
                vec![special_fx_stop(s.current_time(), ship, "effects.CloakRegardless")],
            );
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
        return Err(CallError::User { msg: messages::MSG_CUSTOM_NOTIFY.into(), dict: vec![("notify".into(), Value::unicode(messages::MUST_BE_IN_SPACE))] });
    };
    c.world.system(dest_system).await.map_err(CallError::Internal)?;
    super::combat::reset(c);
    c.world.remove_ball(system, ship).await;
    item_dao::position(&c.db, ship, dest_system, pos).await.map_err(db_err)?;
    let ball = ship_ball(c, ship, pos).await?;
    if dest_system == system {
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
        return send_state(c).await;
    }
    let (constellation, region) = ancestry(&c.sde, dest_system)?;
    c.world.unobserve(c.login.client_id);
    character_dao::location(&c.db, char_id, dest_system, constellation, region).await.map_err(db_err)?;
    c.world.add_ship(dest_system, ball).await.map_err(CallError::Internal)?;
    c.change_session(vec![
        ("solarsystemid", Value::Int(dest_system)),
        ("solarsystemid2", Value::Int(dest_system)),
        ("constellationid", Value::Int(constellation)),
        ("regionid", Value::Int(region)),
    ]);
    let chat = c.chat.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(LOCAL_CHAT_INVITE_DELAY_MS)).await;
        chat.invite(char_id, &format!("local_{dest_system}"));
    });
    info!(user = c.login.user_id, "ship {ship} teleported {system} -> {dest_system}");
    Ok(())
}

/// The pilot's ship was destroyed in space (combat::damage_player): its fittings and
/// cargo go with it and the pilot ejects in a capsule where it died.
pub async fn lose_ship(c: &mut Client, ship: i64, pos: [f64; 3]) -> Result<(), CallError> {
    let (Some(char_id), Some(system)) = (c.session.int("charid"), c.session.int("solarsystemid")) else { return Ok(()) };
    if c.session.int("shipid") != Some(ship) {
        return Ok(());
    }
    let corp = c.session.int("corpid").unwrap_or(0);
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox<()>| {
            let sde = sde.clone();
            Box::pin(async move {
            if !item_dao::lock_reload_ship(tx, ship).await.map_err(db_err)? {
                return Ok(None);
            }
            let lost_type = crate::dao::items::inventory::type_id(tx, ship).await.map_err(db_err)?;
            // The killmail lists what went down with it: fittings, cargo and loaded charges.
            let mut lost: super::crimewatch::Lost = item_dao::lost_items(tx, ship).await.map_err(db_err)?
                .iter().map(|r| (r.type_id, r.flag_id, r.singleton, if r.singleton { 1 } else { r.quantity as i64 })).collect();
            for r in item_dao::lost_charges(tx, ship).await.map_err(db_err)? {
                lost.push((r.type_id, r.flag_id, false, r.quantity as i64));
            }
            let home = if lost_type == Some(TYPE_CAPSULE) {
                let r = super::character::home_station::home(tx, &sde, char_id).await?;
                Some((r.station_id, r.solar_system_id, r.constellation_id, r.region_id))
            } else { None };
            item_dao::delete_lost(tx, ship).await.map_err(db_err)?;
            let capsule: i64 = if let Some((station, _, _, _)) = home {
                creation::rookie_ship(tx, TYPE_CAPSULE, char_id, station, FLAG_HANGAR).await.map_err(db_err)?
            } else {
                crate::dao::items::creation::positioned(tx, TYPE_CAPSULE, char_id, system, pos).await.map_err(db_err)?
            };
            if let Some((station, system, constellation, region)) = home {
                character_dao::set_home(tx, char_id, capsule, station, system, constellation, region).await.map_err(db_err)?;
            } else {
                selection::set_ship(tx, char_id, capsule).await.map_err(db_err)?;
            }
            outbox.operation(());
            Ok(Some((capsule, home, lost_type, lost)))
            })
        },
        |result, outbox| async move {
            let Some((capsule, home, lost_type, lost)) = result else { return Ok(()) };
            for entry in outbox.into_entries() {
                match entry {
                    Effect::Notification(notification) => notification.dispatch(&c.world.clone(), Some(&mut *c))?,
                    Effect::Operation(()) => {
                        super::combat::reset(c);
                        super::combat::forget_ship(ship);
                        if let Some(ship_type) = lost_type {
                            match super::crimewatch::ship_lost(&c.db, &c.world, system, char_id, corp, ship, ship_type, &lost).await {
                                Err(CallError::Internal(e)) => warn!(user = c.login.user_id, "killmail for {ship}: {e:#}"),
                                Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "killmail for {ship}: {msg}"),
                                Ok(()) => {}
                            }
                            match super::insurance::ship_destroyed(&c.db, &c.world, ship).await {
                                Err(CallError::Internal(e)) => warn!(user = c.login.user_id, "insurance payout for {ship}: {e:#}"),
                                Err(CallError::User { msg, .. }) => warn!(user = c.login.user_id, "insurance payout for {ship}: {msg}"),
                                Ok(()) => {}
                            }
                        }
                    }
                }
            }
            if let Some((station, system, constellation, region)) = home {
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
            } else {
                let ball = ship_ball(c, capsule, pos).await?;
                c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
                info!(user = c.login.user_id, "ship {ship} lost; {char_id} ejected in capsule {capsule}");
                c.change_session(vec![("shipid", Value::Int(capsule))]);
                send_state(c).await
            }
        },
    ).await
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
/// `min_range` short of the point itself (bookmarks, mission sites, warp-in points),
/// as Ballpark::RealWarp places it.
pub(super) fn warp_to(c: &mut Client, object: Option<(V3, f64)>, point: V3, min_range: f64) -> CallResult {
    movement(c, move |b, t, _| warp_command(b, t, object, point, min_range))
}

fn warp_command(b: &mut Ball, t: i64, object: Option<(V3, f64)>, point: V3, min_range: f64) -> Vec<Value> {
    let delta = space::sub(point, b.pos);
    if delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2] < MIN_WARP_DISTANCE_SQUARED {
        b.mode = MODE_GOTO;
        b.goto = point;
        b.warp = None;
        if b.speed_fraction == 0.0 { b.speed_fraction = 1.0; }
        return vec![event(t, "GotoPoint", vec![Value::Int(b.id), Value::Float(point[0]), Value::Float(point[1]), Value::Float(point[2])])];
    }
    let (center, stand_off) = match object {
        Some((pos, radius)) => (pos, radius + b.radius as f64 + min_range.max(2500.0)),
        None => (point, min_range.max(0.0)),
    };
    let away = [b.pos[0] - center[0], b.pos[1] - center[1], b.pos[2] - center[2]];
    let l = (away[0] * away[0] + away[1] * away[1] + away[2] * away[2]).sqrt().max(1.0);
    let to = [center[0] + away[0] / l * stand_off, center[1] + away[1] / l * stand_off, center[2] + away[2] / l * stand_off];
    let distance = ((to[0] - b.pos[0]).powi(2) + (to[1] - b.pos[1]).powi(2) + (to[2] - b.pos[2]).powi(2)).sqrt();
    let tau = (b.mass * b.agility as f64 / 1e6).max(0.1);
    let align = -tau * 0.25f64.ln();
    b.mode = MODE_WARP;
    b.goto = to;
    b.speed_fraction = 1.0;
    b.warp = Some(Warp { from: b.pos, to, start: b.last_ms, duration: seconds_to_ms_f64(align + warp_duration(b.warp_factor as f64, distance)), factor: b.warp_factor });
    b.effect_stamp = t;
    // Destiny only enters warp once aligned at 75% of max velocity, and thrust scales
    // with the speed fraction (Ball::IsAlignedForWarp, GotoThrust): warp at full throttle,
    // or a stopped ship sits aligning until the 180 s MAX_ALIGN_TICKS fallback.
    // effects.Warping drives the tunnel, camera and audio (and WarpOut for onlookers);
    // it follows the ball through align and warp and the client stops it on exit.
    // `to` already includes min_range; Ballpark::RealWarp would pull the landing point
    // back by minRange again, so the client gets 0.
    vec![
        event(t, "WarpTo", vec![Value::Int(b.id), Value::Float(to[0]), Value::Float(to[1]), Value::Float(to[2]), Value::Float(0.0), Value::Int(b.warp_factor)]),
        event(t, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]),
        special_fx(t, b.id, None, None, "effects.Warping", None),
    ]
}

/// Ensure a stationary ship starts moving when given a heading (client sends 1.0 separately too).
fn ensure_moving(b: &mut Ball, tick: i64, ev: &mut Vec<Value>) {
    if b.speed_fraction <= 0.0 {
        b.speed_fraction = 1.0;
        ev.push(event(tick, "SetSpeedFraction", vec![Value::Int(b.id), Value::Float(1.0)]));
    }
}

mod ship;
mod beyonce;

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "ship" => ship::dispatch(c, call).await,
        "beyonce" => beyonce::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
