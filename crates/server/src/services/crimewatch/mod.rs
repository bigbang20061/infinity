//! Crimewatch: combat timers, criminal flags, engagements, security status, CONCORD and
//! killmails (eve/client/script/ui/services/crimewatchSvc.py, crimewatch.const).
//!
//! Activating a weapon on another pilot's ship is legal against criminals or suspects, configured
//! outlaws, pilots you are engaged with, and anywhere outside empire space. Otherwise it is a criminal
//! act: the aggressor loses security status and is flagged criminal; in configured high-sec
//! CONCORD destroys their ship after a response time that grows as security falls, unless
//! CONCORD responses are disabled. Aggression starts weapons and PvP timers and a mutual
//! engagement regardless; their durations are operator policies.
//!
//! A lost player ship writes a killmail (game.killmails) naming attackers within the configured
//! window; an illegal kill costs those aggressors more security status. Killing NPCs that carry
//! a bounty slowly raises it. Security adjustments and CONCORD response delays are configurable.
//! CONCORD responses use the crime system's actor timer; character expiry timers
//! follow pilots across systems and stations on Tokio tasks. These timers and flags
//! are in memory; at startup active persisted kill rights restore their suspect flags
//! without extending their deadlines. Other timers and flags are lost on restart.
//! Killmail records, kill-report notifications and security penalties commit together;
//! damage contributions remain in memory until that transaction commits.
//! System security comes from the startup SDE map; character security is persisted in game data.
mod corp_registry;
mod crimewatch;
mod war_statistic_mgr;

use super::{db_err, keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, ms_from_filetime_ticks, Client};
use crate::consts::FILETIME_SECOND;
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::{seconds_to_ms, World};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use tracing::{info, warn};

// crimewatch.const
const WEAPONS_IDLE: i64 = 100;
const WEAPONS_TIMER: i64 = 102;
const PVP_IDLE: i64 = 200;
const PVP_TIMER: i64 = 202;
const CRIMINAL_IDLE: i64 = 300;
const CRIMINAL_TIMER: i64 = 303;
const SUSPECT_TIMER: i64 = 304;
const NPC_IDLE: i64 = 400;
const DISAPPROVAL_IDLE: i64 = 500;
const SAFETY_LEVEL_FULL: i64 = 2;

/// Existing CONCORD response formula's minimum security input, distinct from the high-sec threshold.
const CONCORD_MIN_SECURITY: f64 = 0.5;
/// Existing server grace period after a character timer's expiry.
const CHARACTER_TIMER_GRACE_MS: u64 = 500;
// notificationconst
const NOTIFICATION_KILL_REPORT_VICTIM: i32 = 140;
const NOTIFICATION_KILL_REPORT_FINAL_BLOW: i32 = 141;
const NOTIFICATION_GROUP_WAR: i32 = 8;

#[derive(Default)]
struct Pilot {
    weapons_until: i64,
    pvp_until: i64,
    criminal_until: i64,
    suspect_until: i64,
}

/// An attacker's share of the damage to a ship.
#[derive(Clone, Default, PartialEq)]
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

/// Owns a pending response until its timer runs or is discarded.
struct PendingConcord(i64);

impl Drop for PendingConcord {
    fn drop(&mut self) {
        state().concord.remove(&self.0);
    }
}

/// When the pilot's weapons timer runs out, if it is running.
pub fn weapons_timer(char_id: i64) -> Option<i64> {
    let until = state().pilots.get(&char_id).map(|p| p.weapons_until)?;
    (until > filetime_now()).then_some(until)
}

fn is_criminal(char_id: i64, now: i64) -> bool {
    state().pilots.get(&char_id).is_some_and(|p| p.criminal_until > now || p.suspect_until > now)
}

pub async fn security_status(db: &(impl Executor + ?Sized), char_id: i64) -> Result<f64, CallError> {
    Ok(crate::dao::characters::crimewatch::security(db, char_id).await.map_err(db_err)?.unwrap_or(0.0))
}

fn system_security(world: &World, system: i64) -> f64 {
    world.sde().map.system(system).and_then(|s| s.security).unwrap_or(0.0)
}

/// Move a pilot's security status `fraction` of the way to `toward` (+10 or -10).
async fn shift_security<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, fraction: f64, toward: f64) -> Result<f64, CallError> {
    let new = crate::dao::characters::crimewatch::shift_security(db, char_id, fraction, toward).await.map_err(db_err)?;
    outbox.notify_pilot(char_id, "OnSecurityStatusUpdate", vec![Value::Float(new)]);
    Ok(new)
}

/// Pilots in space in a system.
async fn pilots_in(world: &World, system: i64) -> Vec<i64> {
    world.with(system, move |s| s.pilots().filter_map(|b| b.char_id).collect()).await.unwrap_or_default()
}

fn timer(state: i64, until: Option<i64>) -> Value {
    Value::tuple([Value::Int(state), until.map(Value::Int).unwrap_or(Value::None)])
}

/// Tell a pilot about one of their timers now, and again (idle) once it runs out.
fn announce(world: &Arc<World>, char_id: i64, event: &'static str, running: i64, idle: i64, until: i64) {
    world.notify_pilot(char_id, event, vec![Value::Int(running), Value::Int(until)]);
    let world = world.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(ms_from_filetime_ticks((until - filetime_now()).max(0)) as u64 + CHARACTER_TIMER_GRACE_MS)).await;
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
    let policy = &crate::config::game().crimewatch;
    let system_sec = system_security(world, system);
    let victim_sec = security_status(db, victim).await?;
    let legal = system_sec <= 0.0
        || is_criminal(victim, now)
        || victim_sec <= policy.outlaw_threshold
        || state().engagements.get(&(attacker, victim)).is_some_and(|until| *until > now);
    let (weapons, pvp) = (
        now.saturating_add(policy.weapons_timer_secs.saturating_mul(FILETIME_SECOND)),
        now.saturating_add(policy.pvp_timer_secs.saturating_mul(FILETIME_SECOND)),
    );
    let until = now.saturating_add(policy.criminal_timer_secs.saturating_mul(FILETIME_SECOND));
    let engagement = now.saturating_add(policy.engagement_secs.saturating_mul(FILETIME_SECOND));
    let became_criminal = {
        let mut st = state();
        let a = st.pilots.entry(attacker).or_default();
        a.weapons_until = weapons;
        a.pvp_until = pvp;
        let became = !legal && a.criminal_until <= now;
        if !legal {
            a.criminal_until = until;
        }
        st.pilots.entry(victim).or_default().pvp_until = pvp;
        st.engagements.insert((victim, attacker), engagement);
        st.engagements.insert((attacker, victim), engagement);
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
    world.notify_pilot(attacker, "OnCrimewatchEngagementCreated", vec![Value::Int(victim), Value::Int(engagement)]);
    world.notify_pilot(victim, "OnCrimewatchEngagementCreated", vec![Value::Int(attacker), Value::Int(engagement)]);
    if legal {
        return Ok(());
    }
    if let Err(error) = super::bounty::kill_rights::earn(db, world, victim, attacker).await {
        match error {
            CallError::Internal(error) => warn!("crimewatch: earning kill right failed: {error:#}"),
            CallError::User { msg, .. } => warn!("crimewatch: earning kill right refused: {msg}"),
        }
    }
    world.notify_pilot(attacker, "OnCriminalTimerUpdate", vec![Value::Int(CRIMINAL_TIMER), Value::Int(until)]);
    let mut outbox = Outbox::default();
    shift_security(db, &mut outbox, attacker, policy.security_loss_aggression, -10.0).await?;
    dispatch_world((), outbox, world)?;
    info!("crimewatch: {attacker} committed a criminal act against {victim} in {system} (security {system_sec:.1})");
    if became_criminal {
        for pilot in pilots_in(world, system).await {
            world.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", vec![Value::list([]), Value::list([]), Value::list([Value::Int(attacker)])]);
        }
        clear_flag_later(db.clone(), world.clone(), attacker, until, Flag::Criminal);
    }
    if reserve_concord(policy.concord_enabled, system_sec, policy.highsec_threshold, attacker) {
        dispatch_concord(world.clone(), system, attacker, system_sec);
    }
    Ok(())
}

fn reserve_concord(enabled: bool, security: f64, highsec_threshold: f64, attacker: i64) -> bool {
    enabled && security >= highsec_threshold && state().concord.insert(attacker)
}

#[derive(Clone, Copy)]
enum Flag { Criminal, Suspect }

impl Flag {
    fn until(self, pilot: &Pilot) -> i64 {
        match self { Self::Criminal => pilot.criminal_until, Self::Suspect => pilot.suspect_until }
    }
}

/// Resolve the effective flag without hiding a still-active criminal or suspect.
fn effective_flag(pilot: &Pilot, now: i64) -> (usize, i64, Option<i64>) {
    if pilot.criminal_until > now {
        (2, CRIMINAL_TIMER, Some(pilot.criminal_until))
    } else if pilot.suspect_until > now {
        (1, SUSPECT_TIMER, Some(pilot.suspect_until))
    } else {
        (0, CRIMINAL_IDLE, None)
    }
}

async fn announce_flag(db: &Db, world: &World, char_id: i64) {
    let (slot, timer_state, until) = {
        let st = state();
        st.pilots.get(&char_id).map(|p| effective_flag(p, filetime_now()))
            .unwrap_or((0, CRIMINAL_IDLE, None))
    };
    world.notify_pilot(char_id, "OnCriminalTimerUpdate", vec![Value::Int(timer_state), until.map(Value::Int).unwrap_or(Value::None)]);
    let system = match crate::dao::characters::crimewatch::solar_system(db, char_id).await {
        Ok(system) => system,
        Err(e) => { warn!("crimewatch: locating {char_id}: {e}"); None }
    };
    if let Some(system) = system {
        for pilot in pilots_in(world, system).await {
            let mut flags = vec![Value::list([]), Value::list([]), Value::list([])];
            flags[slot] = Value::list([Value::Int(char_id)]);
            world.notify_pilot(pilot, "OnSystemCriminalFlagUpdates", flags);
        }
    }
}

/// Apply a suspect flag for a paid or owner-activated kill right.
pub async fn flag_suspect(db: &Db, world: &Arc<World>, char_id: i64, until: i64) {
    {
        let mut st = state();
        let pilot = st.pilots.entry(char_id).or_default();
        pilot.suspect_until = pilot.suspect_until.max(until);
    }
    announce_flag(db, world, char_id).await;
    clear_flag_later(db.clone(), world.clone(), char_id, until, Flag::Suspect);
}

/// Lift an expired flag while preserving any other active flag.
fn clear_flag_later(db: Db, world: Arc<World>, char_id: i64, until: i64, flag: Flag) {
    tokio::spawn(async move {
        let mut until = until;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(ms_from_filetime_ticks((until - filetime_now()).max(0)) as u64 + CHARACTER_TIMER_GRACE_MS)).await;
            let current = state().pilots.get(&char_id).map(|p| flag.until(p)).unwrap_or(0);
            if current > filetime_now() {
                until = current;
                continue;
            }
            break;
        }
        announce_flag(&db, &world, char_id).await;
    });
}

/// CONCORD destroys the criminal's ship after the configured security-dependent delay.
fn dispatch_concord(world: Arc<World>, system: i64, criminal: i64, security: f64) {
    let policy = &crate::config::game().crimewatch;
    let delay_seconds = concord_delay_seconds(security, policy.concord_base_delay_secs, policy.concord_delay_per_security_secs);
    let pending = PendingConcord(criminal);
    let weak_world = Arc::downgrade(&world);
    world.schedule(system, seconds_to_ms(delay_seconds as f64), concord_response(pending, system, weak_world));
}

fn concord_delay_seconds(security: f64, base: i64, slope: i64) -> u64 {
    (base as f64 + (1.0 - security.clamp(CONCORD_MIN_SECURITY, 1.0)) * slope as f64) as u64
}

fn concord_response(pending: PendingConcord, system: i64, weak_world: Weak<World>) -> impl FnOnce(&mut crate::space::System) + Send + 'static {
    move |s| {
        let criminal = pending.0;
        drop(pending);
        if let Some(ship) = s.pilot_ball(criminal).map(|b| b.id) {
            info!("crimewatch: CONCORD destroyed ship {ship} of {criminal} in {system}");
            s.notify_pilot(
                criminal,
                "OnRemoteMessage",
                vec![Value::str(messages::MSG_CUSTOM_NOTIFY), Value::dict([("notify", Value::unicode(messages::CONCORD_DESTROYED_SHIP_FOR_CRIME))])],
            );
            s.defer(async move {
                if let Some(world) = weak_world.upgrade() {
                    super::combat::destroy_player_ship(&world, system, ship, criminal).await;
                }
            });
        }
    }
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
    let snapshot = state().damage.get(&ship).cloned().unwrap_or_default();
    let attackers: Vec<(i64, Contribution)> = snapshot.iter()
        .filter(|(_, c)| c.last >= now.saturating_sub(crate::config::game().crimewatch.killmail_attacker_window_secs.saturating_mul(FILETIME_SECOND)))
        .map(|(id, c)| (*id, c.clone()))
        .collect();
    let attacker_count = attackers.len();
    let items = items.clone();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let mut attackers = attackers.clone();
            let items = items.clone();
            Box::pin(async move {
                write_killmail(tx, outbox, system, victim, victim_corp, ship_type, &items, &mut attackers, now).await
            })
        },
        |(kill_id, isk_lost), outbox| async move {
            {
                let mut st = state();
                if let Some(current) = st.damage.get_mut(&ship) {
                    current.retain(|id, contribution| snapshot.get(id) != Some(contribution));
                    if current.is_empty() {
                        st.damage.remove(&ship);
                    }
                }
            }
            info!("crimewatch: killmail {kill_id} for {victim}'s ship type {ship_type} in {system} ({attacker_count} attackers, {isk_lost:.0} ISK)");
            crate::scheduler::wake();
            dispatch_world((), outbox, world)
        },
    ).await
}

#[allow(clippy::too_many_arguments)]
async fn write_killmail<X: Send>(
    db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, system: i64, victim: i64,
    victim_corp: i64, ship_type: i32, items: &Lost, attackers: &mut [(i64, Contribution)], now: i64,
) -> Result<(i64, f64), CallError> {
    let final_blow = attackers.iter().max_by_key(|(_, c)| c.last).cloned();
    let mut blob = String::from("<doc><attackers>");
    for (char_id, c) in attackers.iter() {
        let corp = crate::dao::characters::crimewatch::corporation(db, *char_id).await.map_err(db_err)?.unwrap_or(0);
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
    let prices = crate::dao::killmails::prices(db, &types).await.map_err(db_err)?;
    let price: HashMap<i32, f64> = prices.iter().map(|r| (r.type_id, r.average)).collect();
    let isk_lost = price.get(&ship_type).copied().unwrap_or(0.0) + items.iter().map(|(t, _, _, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum::<f64>();
    let damage_taken: f64 = attackers.iter().map(|(_, c)| c.damage).sum();
    let (final_char, final_c) = match &final_blow {
        Some((ch, c)) => (Some(*ch), c.clone()),
        None => (None, Contribution::default()),
    };
    let final_corp: Option<i64> = match final_char {
        Some(ch) => crate::dao::characters::crimewatch::corporation(db, ch).await.map_err(db_err)?,
        None => None,
    };
    let final_sec = match final_char {
        Some(ch) => security_status(db, ch).await?,
        None => 0.0,
    };
    let involved: Vec<i64> = attackers.iter().map(|(ch, _)| *ch).collect();
    let kill_id = crate::dao::killmails::insert(db, &crate::dao::killmails::NewKillmail {
        now, system, victim, victim_corp, ship_type, damage_taken, final_char, final_corp,
        final_ship_type: final_c.ship_type, final_weapon_type: final_c.weapon_type, final_damage: final_c.damage,
        final_sec, isk_lost, involved: &involved, blob: &blob,
    }).await.map_err(db_err)?;
    super::bounty::kill_rights::consume(db, outbox, victim, now).await?;
    if let Some(final_char) = final_char {
        super::bounty::payout::schedule(db, super::bounty::payout::Payload {
            killmail_id: kill_id, victim, victim_corp, ship_type, final_char, isk_lost,
        }, now).await?;
    }

    // Kill reports (notificationconst groupWar): the victim's, and the final blow's.
    super::mail::notify(db, outbox, victim, NOTIFICATION_KILL_REPORT_VICTIM, NOTIFICATION_GROUP_WAR, victim, &format!("{{killMailHash: '', killMailID: {kill_id}}}"))
        .await?;
    if let Some(killer) = final_char {
        let data = format!("{{killMailHash: '', killMailID: {kill_id}, victimID: {victim}, victimShipTypeID: {ship_type}}}");
        super::mail::notify(db, outbox, killer, NOTIFICATION_KILL_REPORT_FINAL_BLOW, NOTIFICATION_GROUP_WAR, victim, &data).await?;
    }
    attackers.sort_unstable_by_key(|(char_id, _)| *char_id);
    for (char_id, c) in attackers {
        if c.illegal {
            shift_security(db, outbox, *char_id, crate::config::game().crimewatch.security_loss_kill, -10.0).await?;
        }
    }
    Ok((kill_id, isk_lost))
}

/// A pilot killed an NPC with a bounty.
pub async fn npc_killed(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    let mut outbox = Outbox::default();
    shift_security(db, &mut outbox, char_id, crate::config::game().crimewatch.security_gain_npc_kill, 10.0).await?;
    dispatch_world((), outbox, world)
}



/// Killmails the character was involved in (`kills`) or the victim of, newest first.
async fn killmails(c: &Client, kills: Option<bool>, key: i64, num: i64, before: Option<i64>) -> CallResult {
    let filter = match kills {
        Some(true) => crate::dao::killmails::Filter::Kills,
        Some(false) => crate::dao::killmails::Filter::Losses,
        None => crate::dao::killmails::Filter::Id,
    };
    let rows = crate::dao::killmails::list(&c.db, filter, key, before, num.clamp(1, 500)).await.map_err(db_err)?;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Ok(Value::list(rows.into_iter().map(|r| {
        keyval([
            ("killID", Value::Int(r.kill_id)),
            ("killTime", Value::Int(r.kill_time)),
            ("solarSystemID", Value::Int(r.solar_system_id)),
            ("moonID", Value::None),
            ("victimCharacterID", Value::Int(r.victim_id)),
            ("victimCorporationID", Value::Int(r.victim_corp_id)),
            ("victimAllianceID", Value::None),
            ("victimFactionID", Value::None),
            ("victimShipTypeID", Value::Int(r.victim_ship_type as i64)),
            ("victimDamageTaken", Value::Int(r.damage_taken.round() as i64)),
            ("finalCharacterID", opt(r.final_char_id)),
            ("finalCorporationID", opt(r.final_corp_id)),
            ("finalAllianceID", Value::None),
            ("finalFactionID", Value::None),
            ("finalShipTypeID", Value::Int(r.final_ship_type as i64)),
            ("finalWeaponTypeID", Value::Int(r.final_weapon_type as i64)),
            ("finalDamageDone", Value::Int(r.final_damage.round() as i64)),
            ("finalSecurityStatus", Value::Float(r.final_sec_status)),
            ("iskLost", Value::Float(r.isk_lost)),
            ("warID", Value::None),
            ("killBlob", Value::unicode(r.blob)),
        ])
    })))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "crimewatch" => crimewatch::dispatch(c, call).await,
        "corpRegistry" => corp_registry::dispatch(c, call).await,
        "warStatisticMgr" => war_statistic_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
