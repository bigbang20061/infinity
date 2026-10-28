//! Crimewatch: combat timers, criminal flags, engagements, security status, CONCORD and
//! killmails (eve/client/script/ui/services/crimewatchSvc.py, crimewatch.const).
//!
//! Activating a weapon on another pilot's ship is legal against criminals, outlaws (security
//! status -5 or below), pilots you are engaged with, and anywhere outside empire space.
//! Otherwise it is a criminal act: the aggressor loses security status and is flagged criminal
//! for 15 minutes, open to anyone; in high-sec CONCORD destroys their ship after a response
//! time that grows as security falls. Every aggression starts the aggressor's weapons timer
//! (no docking or gate jumps for 60 s), puts both pilots on a 15-minute PvP timer and engages
//! them for 5 minutes, so the victim may shoot back.
//!
//! A lost player ship writes a killmail (game.killmails) naming everyone who damaged it in the
//! last 5 minutes; an illegal kill costs those aggressors more security status. Killing NPCs
//! that carry a bounty slowly raises it.

use super::{keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::db::Db;
use crate::space::World;
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{info, warn};

const SEC: i64 = 10_000_000;
const MINUTE: i64 = 60 * SEC;

// crimewatch.const
const WEAPONS_IDLE: i64 = 100;
const WEAPONS_TIMER: i64 = 102;
const PVP_IDLE: i64 = 200;
const PVP_TIMER: i64 = 202;
const CRIMINAL_IDLE: i64 = 300;
const CRIMINAL_TIMER: i64 = 303;
const NPC_IDLE: i64 = 400;
const DISAPPROVAL_IDLE: i64 = 500;
const WEAPONS_TIMEOUT: i64 = 60 * SEC;
const PVP_TIMEOUT: i64 = 15 * MINUTE;
const CRIMINAL_TIMEOUT: i64 = 15 * MINUTE;
const ENGAGEMENT_DURATION: i64 = 5 * MINUTE;
const SAFETY_LEVEL_FULL: i64 = 2;

/// Security status at or below which anyone may shoot a pilot.
const OUTLAW: f64 = -5.0;
/// Systems at or above this security (0.5 once rounded) are high-sec, where CONCORD answers.
const HIGH_SEC: f64 = 0.45;
/// Fraction of the way to -10 an illegal aggression, and an illegal kill, cost.
const AGGRESSION_PENALTY: f64 = 0.025;
const KILL_PENALTY: f64 = 0.05;
/// Fraction of the way to +10 an NPC kill with a bounty earns.
const NPC_KILL_GAIN: f64 = 0.0005;
/// Damage older than this doesn't put an attacker on the killmail.
const KILLMAIL_WINDOW: i64 = 5 * MINUTE;
// notificationconst
const NOTIFICATION_KILL_REPORT_VICTIM: i32 = 140;
const NOTIFICATION_KILL_REPORT_FINAL_BLOW: i32 = 141;
const NOTIFICATION_GROUP_WAR: i32 = 8;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

#[derive(Default)]
struct Pilot {
    weapons_until: i64,
    pvp_until: i64,
    criminal_until: i64,
}

/// An attacker's share of the damage to a ship.
#[derive(Clone, Default)]
struct Contribution {
    damage: f64,
    ship_type: i32,
    weapon_type: i32,
    last: i64,
    illegal: bool,
}

#[derive(Default)]
struct State {
    pilots: HashMap<i64, Pilot>,
    /// (a, b) -> until when a may shoot b without consequence.
    engagements: HashMap<(i64, i64), i64>,
    /// Victim ship -> attacker -> damage done.
    damage: HashMap<i64, HashMap<i64, Contribution>>,
    /// Criminals CONCORD is on its way to.
    concord: HashSet<i64>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

/// When the pilot's weapons timer runs out, if it is running.
pub fn weapons_timer(char_id: i64) -> Option<i64> {
    let until = state().pilots.get(&char_id).map(|p| p.weapons_until)?;
    (until > filetime_now()).then_some(until)
}

fn is_criminal(char_id: i64, now: i64) -> bool {
    state().pilots.get(&char_id).is_some_and(|p| p.criminal_until > now)
}

pub async fn security_status(db: &Db, char_id: i64) -> Result<f64, CallError> {
    Ok(db
        .client
        .query_opt("SELECT security_status FROM game.characters WHERE character_id = $1", &[&char_id])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0))
}

async fn system_security(db: &Db, system: i64) -> Result<f64, CallError> {
    Ok(db
        .client
        .query_opt("SELECT COALESCE(security, 0)::float8 FROM sde.map_solar_systems WHERE solar_system_id = $1::bigint", &[&system])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0.0))
}

/// Move a pilot's security status `fraction` of the way to `toward` (+10 or -10).
async fn shift_security(db: &Db, world: &World, char_id: i64, fraction: f64, toward: f64) -> Result<f64, CallError> {
    let new: f64 = db
        .client
        .query_one(
            "UPDATE game.characters SET security_status = GREATEST(-10, LEAST(10, security_status + ($3 - security_status) * $2))
             WHERE character_id = $1 RETURNING security_status",
            &[&char_id, &fraction, &toward],
        )
        .await
        .map_err(db_err)?
        .get(0);
    world.notify_pilot(char_id, "OnSecurityStatusUpdate", vec![Value::Float(new)]);
    Ok(new)
}

/// Pilots in space in a system.
fn pilots_in(world: &World, system: i64) -> Vec<i64> {
    world.balls_snapshot(system).map(|b| b.values().filter_map(|b| b.char_id).collect()).unwrap_or_default()
}

fn timer(state: i64, until: Option<i64>) -> Value {
    Value::tuple([Value::Int(state), until.map(Value::Int).unwrap_or(Value::None)])
}

/// Tell a pilot about one of their timers now, and again (idle) once it runs out.
fn announce(world: &Arc<World>, char_id: i64, event: &'static str, running: i64, idle: i64, until: i64) {
    world.notify_pilot(char_id, event, vec![Value::Int(running), Value::Int(until)]);
    let world = world.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(((until - filetime_now()).max(0) / 10_000) as u64 + 500)).await;
        let current = {
            let st = state();
            st.pilots.get(&char_id).map(|p| match event {
                "OnWeaponsTimerUpdate" => p.weapons_until,
                "OnPvpTimerUpdate" => p.pvp_until,
                _ => p.criminal_until,
            })
        };
        if current.is_some_and(|c| c <= filetime_now()) {
            world.notify_pilot(char_id, event, vec![Value::Int(idle), Value::None]);
        }
    });
}

/// `attacker` activated a weapon on `victim`'s ship in `system`.
#[allow(clippy::too_many_arguments)]
pub async fn aggress(db: &Db, world: &Arc<World>, system: i64, attacker: i64, attacker_ship_type: i32, weapon_type: i32, victim: i64, victim_ship: i64) -> Result<(), CallError> {
    if attacker == victim {
        return Ok(());
    }
    let now = filetime_now();
    let system_sec = system_security(db, system).await?;
    let victim_sec = security_status(db, victim).await?;
    let legal = system_sec <= 0.0
        || is_criminal(victim, now)
        || victim_sec <= OUTLAW
        || state().engagements.get(&(attacker, victim)).is_some_and(|until| *until > now);
    let (weapons, pvp) = (now + WEAPONS_TIMEOUT, now + PVP_TIMEOUT);
    let became_criminal = {
        let mut st = state();
        let a = st.pilots.entry(attacker).or_default();
        a.weapons_until = weapons;
        a.pvp_until = pvp;
        let became = !legal && a.criminal_until <= now;
        if !legal {
            a.criminal_until = now + CRIMINAL_TIMEOUT;
        }
        st.pilots.entry(victim).or_default().pvp_until = pvp;
        st.engagements.insert((victim, attacker), now + ENGAGEMENT_DURATION);
        st.engagements.insert((attacker, victim), now + ENGAGEMENT_DURATION);
        let c = st.damage.entry(victim_ship).or_default().entry(attacker).or_default();
        c.ship_type = attacker_ship_type;
        c.weapon_type = weapon_type;
        c.last = now;
        c.illegal |= !legal;
        became
    };
    announce(world, attacker, "OnWeaponsTimerUpdate", WEAPONS_TIMER, WEAPONS_IDLE, weapons);
    announce(world, attacker, "OnPvpTimerUpdate", PVP_TIMER, PVP_IDLE, pvp);
    announce(world, victim, "OnPvpTimerUpdate", PVP_TIMER, PVP_IDLE, pvp);
    world.notify_pilot(attacker, "OnCrimewatchEngagementCreated", vec![Value::Int(victim), Value::Int(now + ENGAGEMENT_DURATION)]);
    world.notify_pilot(victim, "OnCrimewatchEngagementCreated", vec![Value::Int(attacker), Value::Int(now + ENGAGEMENT_DURATION)]);
    if legal {
        return Ok(());
    }
    let until = now + CRIMINAL_TIMEOUT;
    announce(world, attacker, "OnCriminalTimerUpdate", CRIMINAL_TIMER, CRIMINAL_IDLE, until);
    shift_security(db, world, attacker, AGGRESSION_PENALTY, -10.0).await?;
    info!("crimewatch: {attacker} committed a criminal act against {victim} in {system} (security {system_sec:.1})");
    if became_criminal {
        for pilot in pilots_in(world, system) {
            world.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", vec![Value::list([]), Value::list([]), Value::list([Value::Int(attacker)])]);
        }
        clear_flag_later(db.clone(), world.clone(), attacker, until);
    }
    if system_sec >= HIGH_SEC && state().concord.insert(attacker) {
        dispatch_concord(world.clone(), system, attacker, system_sec);
    }
    Ok(())
}

/// Lift a criminal flag once it runs out, telling the system the pilot is in by then.
fn clear_flag_later(db: Db, world: Arc<World>, char_id: i64, until: i64) {
    tokio::spawn(async move {
        let mut until = until;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(((until - filetime_now()).max(0) / 10_000) as u64 + 500)).await;
            let current = state().pilots.get(&char_id).map(|p| p.criminal_until).unwrap_or(0);
            if current > filetime_now() {
                until = current;
                continue;
            }
            break;
        }
        let system: Option<i64> = match db.client.query_opt("SELECT solar_system_id FROM game.characters WHERE character_id = $1", &[&char_id]).await {
            Ok(r) => r.and_then(|r| r.get(0)),
            Err(e) => {
                warn!("crimewatch: locating {char_id}: {e}");
                None
            }
        };
        if let Some(system) = system {
            for pilot in pilots_in(&world, system) {
                world.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", vec![Value::list([Value::Int(char_id)]), Value::list([]), Value::list([])]);
            }
        }
    });
}

/// CONCORD arrives (6 s in 1.0 systems, up to 20 s in 0.5) and destroys the criminal's ship.
fn dispatch_concord(world: Arc<World>, system: i64, criminal: i64, security: f64) {
    let delay = (6.0 + (1.0 - security.clamp(0.5, 1.0)) * 28.0) as u64;
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        state().concord.remove(&criminal);
        let ship = world.balls_snapshot(system).and_then(|b| b.values().find(|b| b.char_id == Some(criminal)).map(|b| b.id));
        if let Some(ship) = ship {
            info!("crimewatch: CONCORD destroyed ship {ship} of {criminal} in {system}");
            world.notify_pilot(
                criminal,
                "OnRemoteMessage",
                vec![Value::str("CustomNotify"), Value::dict([("notify", Value::unicode("统合部因你的犯罪行为摧毁了你的舰船。"))])],
            );
            super::combat::destroy_player_ship(&world, system, ship, criminal);
        }
    });
}

/// `attacker`'s weapon did `amount` damage to a player's `ship`.
pub fn note_damage(ship: i64, attacker: i64, amount: f64) {
    let mut st = state();
    let c = st.damage.entry(ship).or_default().entry(attacker).or_default();
    c.damage += amount;
    c.last = filetime_now();
}

/// What was on a lost ship, for its killmail: (typeID, flagID, singleton, quantity).
pub type Lost = Vec<(i32, i32, bool, i64)>;

/// A player's ship was destroyed: write its killmail and make illegal killers pay for it.
#[allow(clippy::too_many_arguments)]
pub async fn ship_lost(db: &Db, world: &World, system: i64, victim: i64, victim_corp: i64, ship: i64, ship_type: i32, items: &Lost) -> Result<(), CallError> {
    let now = filetime_now();
    let attackers: Vec<(i64, Contribution)> = {
        let mut st = state();
        st.damage.remove(&ship).unwrap_or_default().into_iter().filter(|(_, c)| c.last >= now - KILLMAIL_WINDOW).collect()
    };
    let final_blow = attackers.iter().max_by_key(|(_, c)| c.last).cloned();
    let mut blob = String::from("<doc><attackers>");
    for (char_id, c) in &attackers {
        let corp: i64 = db
            .client
            .query_opt("SELECT corporation_id FROM game.characters WHERE character_id = $1", &[char_id])
            .await
            .map_err(db_err)?
            .map(|r| r.get(0))
            .unwrap_or(0);
        let sec = security_status(db, *char_id).await?;
        blob += &format!("<a c={char_id} r={corp} s={} w={} d={} t={sec:.1} />", c.ship_type, c.weapon_type, c.damage.round() as i64);
    }
    blob += "</attackers><items>";
    for (type_id, flag, singleton, quantity) in items {
        blob += &format!("<i t={type_id} f={flag} s={} d=0 x={quantity} />", *singleton as i32);
    }
    blob += "</items></doc>";
    let mut types: Vec<i32> = items.iter().map(|i| i.0).collect();
    types.push(ship_type);
    let prices = db.client.query("SELECT type_id, average FROM game.retail_prices WHERE type_id = ANY($1)", &[&types]).await.map_err(db_err)?;
    let price: HashMap<i32, f64> = prices.iter().map(|r| (r.get(0), r.get(1))).collect();
    let isk_lost = price.get(&ship_type).copied().unwrap_or(0.0) + items.iter().map(|(t, _, _, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum::<f64>();
    let damage_taken: f64 = attackers.iter().map(|(_, c)| c.damage).sum();
    let (final_char, final_c) = match &final_blow {
        Some((ch, c)) => (Some(*ch), c.clone()),
        None => (None, Contribution::default()),
    };
    let final_corp: Option<i64> = match final_char {
        Some(ch) => db.client.query_opt("SELECT corporation_id FROM game.characters WHERE character_id = $1", &[&ch]).await.map_err(db_err)?.map(|r| r.get(0)),
        None => None,
    };
    let final_sec = match final_char {
        Some(ch) => security_status(db, ch).await?,
        None => 0.0,
    };
    let involved: Vec<i64> = attackers.iter().map(|(ch, _)| *ch).collect();
    let kill_id: i64 = db
        .client
        .query_one(
            "INSERT INTO game.killmails (kill_time, solar_system_id, victim_id, victim_corp_id, victim_ship_type, damage_taken, final_char_id,
                                         final_corp_id, final_ship_type, final_weapon_type, final_damage, final_sec_status, isk_lost,
                                         attacker_ids, blob)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING kill_id",
            &[
                &now,
                &system,
                &victim,
                &victim_corp,
                &ship_type,
                &damage_taken,
                &final_char,
                &final_corp,
                &final_c.ship_type,
                &final_c.weapon_type,
                &final_c.damage,
                &final_sec,
                &isk_lost,
                &involved,
                &blob,
            ],
        )
        .await
        .map_err(db_err)?
        .get(0);
    info!("crimewatch: killmail {kill_id} for {victim}'s ship type {ship_type} in {system} ({} attackers, {isk_lost:.0} ISK)", attackers.len());
    // Kill reports (notificationconst groupWar): the victim's, and the final blow's.
    super::mail::notify(db, world, victim, NOTIFICATION_KILL_REPORT_VICTIM, NOTIFICATION_GROUP_WAR, victim, &format!("{{killMailHash: '', killMailID: {kill_id}}}"))
        .await?;
    if let Some(killer) = final_char {
        let data = format!("{{killMailHash: '', killMailID: {kill_id}, victimID: {victim}, victimShipTypeID: {ship_type}}}");
        super::mail::notify(db, world, killer, NOTIFICATION_KILL_REPORT_FINAL_BLOW, NOTIFICATION_GROUP_WAR, victim, &data).await?;
    }
    for (char_id, c) in &attackers {
        if c.illegal {
            shift_security(db, world, *char_id, KILL_PENALTY, -10.0).await?;
        }
    }
    Ok(())
}

/// A pilot killed an NPC with a bounty.
pub async fn npc_killed(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    shift_security(db, world, char_id, NPC_KILL_GAIN, 10.0).await.map(|_| ())
}


/// `GetClientStates` -> (combat timers, engagements, (criminals, suspects) in the system, safety level).
fn client_states(c: &Client) -> Value {
    let me = c.session.int("charid").unwrap_or(0);
    let now = filetime_now();
    let st = state();
    let pilot = st.pilots.get(&me);
    let running = |until: Option<i64>, on: i64, off: i64| match until.filter(|u| *u > now) {
        Some(u) => timer(on, Some(u)),
        None => timer(off, None),
    };
    let timers = Value::tuple([
        running(pilot.map(|p| p.weapons_until), WEAPONS_TIMER, WEAPONS_IDLE),
        running(pilot.map(|p| p.pvp_until), PVP_TIMER, PVP_IDLE),
        timer(NPC_IDLE, None),
        running(pilot.map(|p| p.criminal_until), CRIMINAL_TIMER, CRIMINAL_IDLE),
        timer(DISAPPROVAL_IDLE, None),
    ]);
    let engagements = Value::Dict(st.engagements.iter().filter(|((a, _), until)| *a == me && **until > now).map(|((_, b), until)| (Value::Int(*b), Value::Int(*until))).collect());
    let here: HashSet<i64> = c.session.int("solarsystemid").map(|s| pilots_in(&c.world, s).into_iter().collect()).unwrap_or_default();
    let criminals: Vec<Value> = st.pilots.iter().filter(|(ch, p)| p.criminal_until > now && here.contains(ch)).map(|(ch, _)| Value::Int(*ch)).collect();
    Value::tuple([timers, engagements, Value::tuple([Value::List(criminals), Value::list([])]), Value::Int(SAFETY_LEVEL_FULL)])
}

/// Killmails the character was involved in (`kills`) or the victim of, newest first.
async fn killmails(c: &Client, kills: Option<bool>, key: i64, num: i64, before: Option<i64>) -> CallResult {
    let condition = match kills {
        Some(true) => "$1 = ANY(attacker_ids)",
        Some(false) => "victim_id = $1",
        None => "kill_id = $1",
    };
    let rows = c
        .db
        .client
        .query(
            &format!(
                "SELECT kill_id, kill_time, solar_system_id, victim_id, victim_corp_id, victim_ship_type, damage_taken, final_char_id,
                        final_corp_id, final_ship_type, final_weapon_type, final_damage, final_sec_status, isk_lost, blob
                 FROM game.killmails WHERE {condition} AND ($2::bigint IS NULL OR kill_id < $2) ORDER BY kill_id DESC LIMIT $3"
            ),
            &[&key, &before, &num.clamp(1, 500)],
        )
        .await
        .map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::list(rows.iter().map(|r| {
        keyval([
            ("killID", Value::Int(r.get(0))),
            ("killTime", Value::Int(r.get(1))),
            ("solarSystemID", Value::Int(r.get(2))),
            ("moonID", Value::None),
            ("victimCharacterID", Value::Int(r.get(3))),
            ("victimCorporationID", Value::Int(r.get(4))),
            ("victimAllianceID", Value::None),
            ("victimFactionID", Value::None),
            ("victimShipTypeID", Value::Int(r.get::<_, i32>(5) as i64)),
            ("victimDamageTaken", Value::Int(r.get::<_, f64>(6).round() as i64)),
            ("finalCharacterID", opt(r.get(7))),
            ("finalCorporationID", opt(r.get(8))),
            ("finalAllianceID", Value::None),
            ("finalFactionID", Value::None),
            ("finalShipTypeID", Value::Int(r.get::<_, i32>(9) as i64)),
            ("finalWeaponTypeID", Value::Int(r.get::<_, i32>(10) as i64)),
            ("finalDamageDone", Value::Int(r.get::<_, f64>(11).round() as i64)),
            ("finalSecurityStatus", Value::Float(r.get(12))),
            ("iskLost", Value::Float(r.get(13))),
            ("warID", Value::None),
            ("killBlob", Value::unicode(r.get::<_, String>(14))),
        ])
    })))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let me = c.session.int("charid").unwrap_or(0);
    Some(match (call.service, call.method) {
        ("crimewatch", "GetClientStates") => Ok(client_states(c)),
        ("crimewatch", "GetMySecurityStatus") => security_status(&c.db, c.session.int("charid").unwrap_or(0)).await.map(Value::Float),
        ("crimewatch", "GetCharacterSecurityStatus") => security_status(&c.db, int(0).unwrap_or(0)).await.map(Value::Float),
        ("crimewatch", "GetSecurityStatusTransactions") => Ok(Value::list([])),
        // GetRecentKills(num, offset): offset is the oldest killID already shown.
        ("corpRegistry", "GetRecentKills") => killmails(c, Some(true), me, int(0).unwrap_or(10), int(1)).await,
        ("corpRegistry", "GetRecentLosses") => killmails(c, Some(false), me, int(0).unwrap_or(10), int(1)).await,
        // GetKillMail(killID, hash)
        ("warStatisticMgr", "GetKillMail") => killmails(c, None, int(0).unwrap_or(0), 1, None).await.map(|v| v.items().and_then(|k| k.first().cloned()).unwrap_or(Value::None)),
        _ => return None,
    })
}
