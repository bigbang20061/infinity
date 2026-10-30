//! Exploration: cosmic anomalies and signatures, the sensor overlay, probe and
//! directional scanning, and wormholes.
//!
//! The live server's dungeon layouts never shipped with the client, so a site is a point
//! off a planet's surface tied to one of the build's dungeons for its name, faction and difficulty.
//! Combat sites fill with that faction's NPCs when first warped to and are replaced
//! elsewhere after their spawn group clears and the site lingers briefly. Data and relic
//! sites hold hackable containers (services/hacking) with loot. A wormhole leads to a
//! random system of its target class, where a K162 leads back; both ends share one mass
//! budget and lifetime.
//!
//! Sites are generated per system on first visit and stored in game.exploration_sites;
//! memory holds the systems loaded since startup. Wormholes and spawned containers are put
//! back in space when their system is loaded again. Startup moves legacy exploration and
//! mission sites out of planets, along with exploration overlays and asteroids.
//! Scan completion, probe recovery and site respawns use transient system actor timers;
//! pending timers do not survive a restart.
//! Probe charges are consumed atomically before transient probes are created.
//!
//! Protocol (client: scanSvc, sensorSuiteService, probescanning, explorationscanner):
//! - scanMgr.SignalTrackerRegister() -> OnSignalTrackerFullState(systemID, (anomalies,
//!   signatures, staticSites, structures)); bound scanMgr.GetFullState() returns the tuple.
//! - dogmaIM.LaunchProbes(launcherID, count) -> OnNewProbe(probe) per probe.
//! - bound scanMgr.RequestScans(probes | None) -> OnSystemScanStarted(start, ms, probes),
//!   then OnSystemScanStopped(probeIDs, results, absentTargetIDs).
//! - bound scanMgr.ConeScan(angle, range, x, y, z) -> [record(id, typeID, groupID)].
//! - beyonce.CmdWarpToStuff('scan', targetID); wormholeMgr.WormholeJump(wormholeID).

mod scan_mgr;
mod wormhole_mgr;
mod generation;
mod scanning;
mod sites;

use generation::*;
use scanning::*;
use sites::*;

use super::{db_err, keyval, notify_error, off_surface, rand01, rand_dir, value_ids, Call, CallError, CallResult};
use crate::client::{filetime_now, filetime_ticks_from_seconds, Client};
use crate::consts::{ATTR_EXPLOSION_DELAY, ATTR_REQUIRED_SKILL_1, AU, FACTION_AMARR, FACTION_ANGEL_CARTEL, FACTION_CALDARI, FACTION_GALLENTE, FACTION_GURISTAS, FACTION_MINMATAR, FILETIME_SECOND, FLAG_CARGO, GROUP_ASTEROID_BELT, GROUP_PLANET, SKILL_ARCHAEOLOGY, SKILL_HACKING};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_client, Effect, Outbox};
use crate::messages;
use crate::space::{dist, kg_to_tonnes, minutes_to_seconds, now_ms, Ball, World, MODE_RIGID, MODE_STOP, V3};
use marshal::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tracing::{info, warn};

const GROUP_COSMIC_SIGNATURE: i64 = 502;
/// Design policy: legacy sites closer than this to a planet's surface are moved (m).
const SITE_REPAIR_CLEARANCE: f64 = 1_000_000.0;
const GROUP_COSMIC_ANOMALY: i64 = 885;
// probescanning.const scan groups
const SCAN_GROUP_SIGNATURES: i64 = 3;
const SCAN_GROUP_ANOMALIES: i64 = 8;
// Site kinds by the scan strength attribute that finds them (probescanning.explorationSites).
const STRENGTH_RADAR: i64 = 208;
const STRENGTH_LADAR: i64 = 209;
const STRENGTH_MAGNETOMETRIC: i64 = 210;
const STRENGTH_GRAVIMETRIC: i64 = 211;
const STRENGTH_ALL: i64 = 1136;
const STRENGTH_WORMHOLE: i64 = 1908;
const ARCHETYPE_COMBAT: &str = "24";
const ARCHETYPE_RELIC: &str = "44";
const ARCHETYPE_DATA: &str = "45";
const ARCHETYPE_ORE: i64 = 27;
const ARCHETYPE_ICE: i64 = 28;
const ARCHETYPE_GAS: i64 = 30;
const GROUP_SCATTER_CONTAINER: i32 = 1207;
const TYPE_K162: i32 = 30831;
const ATTR_BASE_SENSOR_STRENGTH: i32 = 1371;
const ATTR_WH_TARGET_CLASS: i32 = 1381;
const ATTR_WH_LIFE_MINUTES: i32 = 1382;
const ATTR_WH_MASS: i32 = 1383;
const ATTR_WH_JUMP_MASS: i32 = 1385;
// Skills
const SKILL_ASTROMETRICS: i32 = 3412;
const SKILL_RANGEFINDING: i32 = 25739;
const SKILL_ACQUISITION: i32 = 25811;
/// Probes per pilot (probescanning.const.MAX_PROBES).
const MAX_PROBES: usize = 8;
/// A scan's certainty from covering probes: sum of (strength x falloff x ease) / this.
const CERTAINTY_DIVISOR: f64 = 80.0;
/// Server design: scan ease for both ends of a wormhole connection.
const WORMHOLE_SCAN_EASE: f64 = 0.8;
/// Server design: minimum cone direction length for stable normalization.
const CONE_DIRECTION_MIN_LENGTH: f64 = 1e-9;
/// Geometry: a cone's half-angle is its full angle divided by two.
const CONE_HALF_ANGLE_DIVISOR: f64 = 2.0;
/// Pilots jump through a wormhole within this distance of it (m).
const WORMHOLE_JUMP_RANGE: f64 = 5000.0;
/// After a jump, a pilot can't use the same connection again for this long (s).
const POLARIZATION_SECS: i64 = 300;
/// Existing server policy: cleared combat sites linger before replacement.
const CLEARED_SITE_LINGER_MS: u64 = 30_000;
/// Existing server policy: a cleared belt stays empty before rats respawn.
const BELT_RAT_RESPAWN_MS: u64 = 900_000;
/// Existing server policy: fully hacked sites linger for looting.
const HACKED_SITE_LINGER_MS: u64 = 600_000;
/// Existing server policy: recovered probes take this long to disappear.
const PROBE_RECOVERY_DELAY_MS: u64 = 2000;
/// Loot value per hackable container by tier (ISK).
const CONTAINER_LOOT_VALUE: [f64; 4] = [1_500_000.0, 4_000_000.0, 10_000_000.0, 20_000_000.0];
/// Sites sit this far off a planet (m).
const SITE_OFFSET: (f64, f64) = (8_000_000.0, 30_000_000.0);

/// Site IDs (and wormhole ball IDs) continue from game.exploration_site_seq.
static NEXT_SITE_ID: AtomicI64 = AtomicI64::new(0);
static NEXT_PROBE_ID: AtomicI64 = AtomicI64::new(9_600_000_000_000);

fn pick<T: Clone>(items: &[T]) -> Option<T> {
    (!items.is_empty()).then(|| items[(rand01() * items.len() as f64) as usize % items.len()].clone())
}

fn offset(p: V3, d: f64) -> V3 {
    let v = rand_dir();
    [p[0] + v[0] * d, p[1] + v[1] * d, p[2] + v[2] * d]
}

fn pos_value(p: V3) -> Value {
    Value::tuple(p.map(Value::Float))
}

/// A scan target ID like `ABC-123`, unique among `taken`.
fn target_id(taken: &HashSet<String>) -> String {
    loop {
        let letter = || (b'A' + (rand01() * 26.0) as u8 % 26) as char;
        let id = format!("{}{}{}-{:03}", letter(), letter(), letter(), (rand01() * 1000.0) as u32 % 1000);
        if !taken.contains(&id) {
            return id;
        }
    }
}

// ---------------------------------------------------------------- state

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Anomaly,
    Signature,
}

#[derive(Clone)]
struct Wormhole {
    type_id: i32,
    dest_system: i64,
    dest_class: i64,
    /// The site at the other end.
    other: i64,
    born: i64,
    life_minutes: f64,
    mass_max: f64,
    mass_used: f64,
    jump_mass: f64,
    /// The destination region's nebula graphic (slimItem.nebulaType).
    nebula: i64,
}

#[derive(Clone)]
struct Site {
    id: i64,
    target: String,
    system: i64,
    pos: V3,
    kind: Kind,
    strength_attr: i64,
    /// How readily probes resolve it: 1.0 easy, lower is harder.
    ease: f64,
    dungeon_id: Option<i64>,
    name_id: Option<i64>,
    archetype: Option<i64>,
    faction: Option<i64>,
    difficulty: i64,
    /// What the sensor overlay shows for a signature before it is scanned.
    overlay_pos: V3,
    deviation: f64,
    wormhole: Option<Wormhole>,
    /// NPCs spawned at the site (None until someone first warps there; not kept over a
    /// restart, they spawn again on the next visit).
    npcs: Option<Vec<i64>>,
    /// The wormhole is in space (it appears once someone warps to it).
    in_space: bool,
    /// Hackable containers (game.items) of a data or relic site, once spawned.
    containers: Option<Vec<i64>>,
    hacked: Vec<i64>,
}

impl Site {
    fn is_hack_site(&self) -> bool {
        matches!(self.strength_attr, STRENGTH_RADAR | STRENGTH_MAGNETOMETRIC)
    }

    /// Ore and ice anomalies and gas signatures hold a field to mine (services/mining);
    /// its rocks are kept in `containers`.
    fn mining_field(&self) -> Option<super::mining::Field> {
        match self.archetype {
            Some(ARCHETYPE_ORE) => Some(super::mining::Field::Ore),
            Some(ARCHETYPE_ICE) => Some(super::mining::Field::Ice),
            Some(ARCHETYPE_GAS) => Some(super::mining::Field::Gas),
            _ => None,
        }
    }
}

struct Probe {
    char_id: i64,
    system: i64,
    type_id: i32,
    pos: V3,
    expiry: i64,
    strength: f64,
}

#[derive(Default)]
struct State {
    sites: HashMap<i64, Site>,
    /// Systems whose sites are in `sites`.
    loaded: HashSet<i64>,
    probes: HashMap<i64, Probe>,
    /// (character, site) scanned down to a warpable result.
    scanned: HashSet<(i64, i64)>,
    /// (character, system) -> result IDs last reported, for absentTargets.
    reported: HashMap<(i64, i64), HashSet<String>>,
    /// (character, connection) -> polarization end (FILETIME); a connection is keyed by
    /// the lower site ID of its two ends.
    polarized: HashMap<(i64, i64), i64>,
    /// Asteroid belts whose rats are in space or that were cleared recently (belt_rats).
    belts: HashSet<i64>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

fn system_sites(system: i64) -> Vec<Site> {
    state().sites.values().filter(|s| s.system == system).cloned().collect()
}

// ---------------------------------------------------------------- systems

struct SystemInfo {
    security: f64,
    faction: Option<i64>,
    region: i64,
    class: i64,
}


/// Sites and mission sites are placed clear of planets' surfaces; ones made before that
/// (measured from the centre, so inside large planets) are moved out at startup, with
/// their scan overlay positions and any asteroids.
pub async fn unbury_sites(db: &Db) -> Result<(), CallError> {
    // (table, key column, has overlay/asteroids)
    for (table, exploration) in [(crate::dao::exploration::EXPLORATION_SITES_TABLE, true), (crate::dao::exploration::AGENT_MISSIONS_TABLE, false)] {
        let rows = crate::dao::exploration::repair_rows(db, exploration, GROUP_PLANET, SITE_REPAIR_CLEARANCE).await.map_err(db_err)?;
        for r in rows {
            let id = r.id;
            let old = r.old;
            let new = off_surface(r.planet, r.radius, site_distance());
            let delta = [new[0] - old[0], new[1] - old[1], new[2] - old[2]];
            if exploration {
                let site: i64 = id.parse().unwrap_or(0);
                crate::dao::exploration::move_exploration(db, site, delta).await.map_err(db_err)?;
            } else {
                crate::dao::exploration::move_mission(db, &id, delta).await.map_err(db_err)?;
            }
            info!("exploration: moved {table} {id} out of a planet");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- generation

struct DungeonPick {
    id: i64,
    name_id: Option<i64>,
    difficulty: i64,
}
impl DungeonPick { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id: r.get("id"), name_id: r.get("name_id"), difficulty: r.get("difficulty") } } }
struct ProbeAttributes { base: f64, lifetime_ms: f64 }
impl ProbeAttributes { fn from_row(r: &tokio_postgres::Row) -> Self { Self { base: r.get("base"), lifetime_ms: r.get("lifetime_ms") } } }
struct LocalizedName { text: Option<String> }
impl LocalizedName { fn from_row(r: &tokio_postgres::Row) -> Self { Self { text: r.get("text") } } }



// ---------------------------------------------------------------- persistence

fn site_from_row(s: crate::dao::exploration::Site) -> Site {
    Site {
        id: s.id, system: s.system, target: s.target,
        kind: if s.kind == 0 { Kind::Anomaly } else { Kind::Signature },
        strength_attr: s.strength_attr, ease: s.ease, dungeon_id: s.dungeon_id,
        name_id: s.name_id, archetype: s.archetype, faction: s.faction,
        difficulty: s.difficulty, pos: s.pos, overlay_pos: s.overlay_pos,
        deviation: s.deviation, in_space: s.in_space, containers: s.containers,
        hacked: s.hacked,
        wormhole: s.wormhole.map(|w| Wormhole {
            type_id: w.type_id, dest_system: w.dest_system, dest_class: w.dest_class,
            other: w.other, born: w.born, life_minutes: w.life_minutes,
            mass_max: w.mass_max, mass_used: w.mass_used, jump_mass: w.jump_mass, nebula: w.nebula,
        }),
        npcs: None,
    }
}

async fn save_site(db: &(impl Executor + ?Sized), s: &Site) -> Result<(), CallError> {
    let site = crate::dao::exploration::SiteWrite {
        id: s.id, system: s.system, target: &s.target,
        kind: if s.kind == Kind::Anomaly { 0 } else { 1 },
        strength_attr: s.strength_attr, ease: s.ease, dungeon_id: s.dungeon_id,
        name_id: s.name_id, archetype: s.archetype, faction: s.faction,
        difficulty: s.difficulty, pos: s.pos, overlay_pos: s.overlay_pos,
        deviation: s.deviation, in_space: s.in_space, containers: s.containers.as_deref(),
        hacked: &s.hacked,
        wormhole: s.wormhole.as_ref().map(|w| crate::dao::exploration::Wormhole {
            type_id: w.type_id, dest_system: w.dest_system, dest_class: w.dest_class,
            other: w.other, born: w.born, life_minutes: w.life_minutes,
            mass_max: w.mass_max, mass_used: w.mass_used, jump_mass: w.jump_mass, nebula: w.nebula,
        }),
    };
    crate::dao::exploration::save_site(db, &site).await.map_err(db_err)
}

/// Save a site's current in-memory state.
async fn persist(db: &Db, site_id: i64) -> Result<(), CallError> {
    let site = state().sites.get(&site_id).cloned();
    match site {
        Some(s) => save_site(db, &s).await,
        None => Ok(()),
    }
}

async fn delete_site(db: &(impl Executor + ?Sized), site_id: i64) -> Result<(), CallError> {
    crate::dao::exploration::delete_site(db, site_id).await.map_err(db_err)
}

/// Has `char_id` scanned this signature down (this session or before)?
async fn is_scanned(db: &Db, char_id: i64, site_id: i64) -> Result<bool, CallError> {
    if state().scanned.contains(&(char_id, site_id)) {
        return Ok(true);
    }
    let found = crate::dao::exploration::is_scanned(db, char_id, site_id).await.map_err(db_err)?;
    if found {
        state().scanned.insert((char_id, site_id));
    }
    Ok(found)
}

async fn mark_scanned(db: &Db, char_id: i64, site_id: i64) -> Result<(), CallError> {
    if !state().scanned.insert((char_id, site_id)) {
        return Ok(());
    }
    crate::dao::exploration::mark_scanned(db, char_id, site_id).await.map_err(db_err)?;
    Ok(())
}



/// dogmaIM.LaunchProbes(launcherID, count): probes leave the launcher's charge stack.
pub async fn launch_probes(c: &mut Client, call: &Call<'_>) -> CallResult {
    let module = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let wanted = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(1).clamp(1, MAX_PROBES as i64) as i32;
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Ok(Value::None);
    };
    let Some(flag) = crate::dao::exploration::module_flag(&c.db, module, ship).await.map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    let Some(loaded) = crate::dao::items::charges::loaded(&c.db, ship, flag).await.map_err(db_err)?
    else {
        return Err(CallError::User { msg: messages::MSG_NO_CHARGES.into(), dict: vec![] });
    };
    let (type_id, have) = (loaded.type_id, loaded.quantity);
    let out = state().probes.values().filter(|p| p.char_id == char_id && p.system == system).count();
    let take = wanted.min(have).min((MAX_PROBES - out.min(MAX_PROBES)) as i32);
    if take <= 0 {
        return Ok(Value::None);
    }
    let attrs = c
        .db
        .query_one(
            "SELECT COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2), 40) AS base,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $3), 3600000) AS lifetime_ms",
            &[&type_id, &ATTR_BASE_SENSOR_STRENGTH, &ATTR_EXPLOSION_DELAY],
        )
        .await
        .map_err(db_err)?;
    let attrs = ProbeAttributes::from_row(&attrs);
    let (base, lifetime_ms) = (attrs.base, attrs.lifetime_ms);
    let (skill, _) = probe_skills(&c.db, char_id).await?;
    let pos = c.world.ball(system, ship).await.map(|b| b.pos).unwrap_or([0.0; 3]);
    let expiry = filetime_now() + (lifetime_ms * 10_000.0) as i64;
    enum Launch { Probes }
    let user_id = c.login.user_id;
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<Launch>| Box::pin(async move {
            let remaining = crate::dao::exploration::consume_probes(tx, ship, flag, type_id, have, take).await.map_err(db_err)?
                .ok_or_else(|| super::notify_error(messages::INSUFFICIENT_PROBE_LAUNCHER_PROBES))?;
            outbox.notify_client("OnModuleAttributeChanges", vec![super::combat::quantity_change(char_id, ship, flag, type_id, remaining as i64, (remaining + take) as i64)]);
            outbox.operation(Launch::Probes);
            Ok::<(), CallError>(())
        }),
        |(), outbox: Outbox<Launch>| async move {
            let world = c.world.clone();
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(&world, Some(&mut *c))?,
                    Effect::Operation(Launch::Probes) => for _ in 0..take {
                        let id = NEXT_PROBE_ID.fetch_add(1, Ordering::Relaxed);
                        let probe = Probe { char_id, system, type_id, pos, expiry, strength: base * skill };
                        c.notify("OnNewProbe", vec![probe_value(id, &probe)]);
                        state().probes.insert(id, probe);
                    },
                }
            }
            Ok(())
        },
    ).await?;
    info!(user = user_id, "launched {take} probes of {type_id} from {module}");
    Ok(Value::None)
}



/// A pilot is warping to `belt`: unless its rats are still about (or it was cleared less
/// than BELT_RAT_RESPAWN_MS ago), the region's pirates spawn there. Other celestials
/// and wormhole space get none.
pub async fn belt_rats(c: &mut Client, system: i64, belt: i64, center: V3) -> Result<(), CallError> {
    if state().belts.contains(&belt) {
        return Ok(());
    }
    let is_belt = c
        .db
        .query_opt("SELECT 1 FROM sde.map_celestials WHERE item_id = $1 AND group_id = $2", &[&belt, &GROUP_ASTEROID_BELT])
        .await
        .map_err(db_err)?
        .is_some();
    let info = system_info(&c.db, system).await?;
    if !is_belt || (1..=6).contains(&info.class) || !state().belts.insert(belt) {
        return Ok(());
    }
    let faction = pirate_faction(info.faction, info.region);
    let groups: Vec<String> = belt_classes(info.security).iter().map(|class| format!("{} {class}", rat_prefix(faction))).collect();
    let spawned = match spawn_rats(c, system, center, faction, &groups, (5_000.0, 20_000.0)).await {
        Ok(s) => s,
        Err(e) => {
            state().belts.remove(&belt);
            return Err(e);
        }
    };
    info!(user = c.login.user_id, "belt {belt} in {system}: spawned {} rats", spawned.len());
    c.world.post(system, move |s| {
        s.spawn_group(spawned, move |s| {
            s.schedule(BELT_RAT_RESPAWN_MS, move |_| {
                state().belts.remove(&belt);
            });
        });
    });
    Ok(())
}

pub(super) fn err_text(e: CallError) -> String {
    match e {
        CallError::Internal(e) => format!("{e:#}"),
        CallError::User { msg, .. } => msg,
    }
}


/// beyonce.CmdWarpToStuff('scan', targetID): warp to an anomaly or a scanned-down signature.
/// Where a scan result is, if the pilot may bookmark it: anomalies, and signatures they
/// scanned down to 100%.
pub(super) async fn scan_result_position(db: &Db, system: i64, char_id: i64, target: &str) -> Result<Option<[f64; 3]>, CallError> {
    let Some(site) = system_sites(system).into_iter().find(|s| s.target == target) else { return Ok(None) };
    if site.kind == Kind::Signature && !is_scanned(db, char_id, site.id).await? {
        return Ok(None);
    }
    Ok(Some(site.pos))
}

pub async fn warp_to_scan(c: &mut Client, target: &str, min_range: f64) -> CallResult {
    let (Some(system), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("charid")) else { return Ok(Value::None) };
    let site = system_sites(system).into_iter().find(|s| s.target == target);
    let Some(site) = site else { return Err(notify_error(messages::SCAN_RESULT_GONE)) };
    if site.kind == Kind::Signature && !is_scanned(&c.db, char_id, site.id).await? {
        return Err(notify_error(messages::SIGNATURE_REQUIRES_FULL_SCAN_FOR_WARP));
    }
    if site.wormhole.is_some() {
        ensure_wormhole_ball(&c.db, &c.world, site.id).await?;
        return super::ship::warp_to(c, Some((site.pos, 3000.0)), site.pos, min_range);
    }
    if let Some(field) = site.mining_field() {
        if site.containers.is_none() {
            let info = system_info(&c.db, system).await?;
            let jspace = (1..=6).contains(&info.class);
            let rocks = super::mining::grow_field(&c.db, &c.world, system, site.id, site.pos, field, info.security, jspace).await?;
            if let Some(s) = state().sites.get_mut(&site.id) {
                s.containers = Some(rocks.clone());
            }
            persist(&c.db, site.id).await?;
            track_site_clearance(c.db.clone(), c.world.clone(), site.id, system, rocks);
        }
    } else if site.is_hack_site() {
        if site.containers.is_none() {
            spawn_containers(c, &site).await?;
        }
    } else if site.npcs.is_none() && site.dungeon_id.is_some() {
        spawn_site_npcs(c, &site).await?;
    }
    super::ship::warp_to(c, None, site.pos, min_range)
}



/// What a hackable container is, for the analyzer and the minigame.
pub(super) struct HackTarget {
    pub tier: i64,
    pub relic: bool,
    pub hacked: bool,
}

pub(super) fn hack_target(container: i64) -> Option<HackTarget> {
    state().sites.values().find(|s| s.containers.as_ref().is_some_and(|c| c.contains(&container))).map(|s| HackTarget {
        tier: s.difficulty,
        relic: s.strength_attr == STRENGTH_MAGNETOMETRIC,
        hacked: s.hacked.contains(&container),
    })
}

/// An unhacked site container can't be opened.
pub(super) fn container_locked(container: i64) -> bool {
    hack_target(container).is_some_and(|t| !t.hacked)
}

/// Show a container as secure (0), being hacked (1) or hacked (2).
pub(super) async fn set_container_state(world: &World, system: i64, container: i64, security_state: i64) {
    let Some(mut ball) = world.remove_ball(system, container).await else { return };
    ball.slim = vec![("hackingSecurityState", Value::Int(security_state))];
    let _ = world.add_ship(system, ball).await;
}

/// A won hack: the container opens to its hacker, and once every container of the site
/// is open the site winds down.
pub(super) async fn container_hacked(db: &Db, world: &Arc<World>, container: i64, char_id: i64) -> Result<(), CallError> {
    let Some(mut site) = state().sites.values().find(|s| s.containers.as_ref().is_some_and(|c| c.contains(&container))).cloned() else {
        return Ok(());
    };
    if !site.hacked.contains(&container) {
        site.hacked.push(container);
    }
    enum Completion { Open(Site) }
    db.transaction(
        move |tx, outbox: &mut Outbox<Completion>| {
            let site = site.clone();
            Box::pin(async move {
                save_site(tx, &site).await?;
                crate::dao::exploration::set_container_owner(tx, container, char_id).await.map_err(db_err)?;
                outbox.operation(Completion::Open(site));
                Ok(())
            })
        },
        |(), outbox: Outbox<Completion>| async move {
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(world, None)?,
                    Effect::Operation(Completion::Open(site)) => {
                        let site_id = site.id;
                        state().sites.insert(site_id, site.clone());
                        set_container_state(world, site.system, container, 2).await;
                        if site.containers.as_ref().is_some_and(|c| c.iter().all(|id| site.hacked.contains(id))) {
                            let (db, completion_world) = (db.clone(), world.clone());
                            world.schedule(site.system, HACKED_SITE_LINGER_MS, move |s| {
                                s.defer(async move {
                                    if let Err(e) = replace_site(&db, &completion_world, site_id).await {
                                        warn!("replacing site {site_id}: {}", err_text(e));
                                    }
                                });
                            });
                        }
                    }
                }
            }
            Ok(())
        },
    ).await
}

// ---------------------------------------------------------------- GM commands (services/slash)

fn gm_error(text: &str) -> CallError {
    CallError::User { msg: messages::MSG_SLASH_ERROR.into(), dict: vec![("reason".into(), Value::unicode(text))] }
}

/// /wh: a wormhole (in this system if there is one), scanned down and in space, with the
/// GM teleported next to it.
pub(super) async fn gm_wormhole(c: &mut Client) -> Result<String, CallError> {
    let (Some(system), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("charid")) else { return Err(gm_error(messages::GM_UNDOCK_FIRST)) };
    ensure_loaded(&c.db, &c.world, system).await?;
    ensure_static_exit(&c.db, &c.world, system).await?;
    let mut here: Vec<Site> = system_sites(system).into_iter().filter(|s| s.wormhole.is_some() && polarization(char_id, s.id).is_none()).collect();
    here.sort_by_key(|s| !s.wormhole.as_ref().is_some_and(|w| (7..=9).contains(&w.dest_class)));
    let site = match here.into_iter().next() {
        Some(s) => s,
        None => {
            let r = crate::dao::exploration::random_wormhole(&c.db).await.map_err(db_err)?
                .ok_or_else(|| gm_error(messages::GM_NO_WORMHOLES))?;
            let (id, other_system) = (r.site_id, r.system_id);
            ensure_loaded(&c.db, &c.world, other_system).await?;
            state().sites.get(&id).cloned().ok_or_else(|| gm_error(messages::GM_WORMHOLE_CLOSED))?
        }
    };
    let w = site.wormhole.clone().ok_or_else(|| gm_error(messages::GM_NOT_WORMHOLE))?;
    mark_scanned(&c.db, char_id, site.id).await?;
    ensure_wormhole_ball(&c.db, &c.world, site.id).await?;
    super::ship::teleport(c, site.system, [site.pos[0] + 6000.0, site.pos[1], site.pos[2]]).await?;
    Ok(messages::gm_wormhole_result(&site.target, w.type_id, site.system, w.dest_system, w.dest_class))
}

/// /scanall: every signature in the system scanned down for the GM.
pub(super) async fn gm_scan_all(c: &mut Client) -> Result<String, CallError> {
    let (Some(system), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("charid")) else { return Err(gm_error(messages::GM_UNDOCK_FIRST)) };
    ensure_loaded(&c.db, &c.world, system).await?;
    let sites = system_sites(system);
    for s in sites.iter().filter(|s| s.kind == Kind::Signature) {
        mark_scanned(&c.db, char_id, s.id).await?;
    }
    let results: Vec<Value> = sites.iter().map(|s| result_value(s, 1.0, 0.0)).collect();
    state().reported.insert((char_id, system), sites.iter().map(|s| s.target.clone()).collect());
    c.notify("OnSystemScanStopped", vec![Value::list([]), Value::List(results), Value::list([])]);
    Ok(messages::gm_sites_scanned(sites.len()))
}

/// /sites: what this system holds.
pub(super) async fn gm_sites(c: &mut Client) -> Result<String, CallError> {
    let Some(system) = c.session.int("solarsystemid") else { return Err(gm_error(messages::GM_UNDOCK_FIRST)) };
    ensure_loaded(&c.db, &c.world, system).await?;
    let mut lines = vec![];
    for s in system_sites(system) {
        let name: Option<String> = match s.name_id {
            Some(id) => c
                .db
                .query_opt("SELECT data->>'text' AS text FROM sde_native.localization_fsd_zh WHERE key = $1::text", &[&id.to_string()])
                .await
                .map_err(db_err)?
                .and_then(|r| LocalizedName::from_row(&r).text),
            None => None,
        };
        let what = match (&s.wormhole, s.strength_attr) {
            (Some(w), _) => messages::gm_wormhole_site(w.type_id, w.dest_system, w.dest_class),
            (None, STRENGTH_RADAR) => messages::GM_SITE_DATA.into(),
            (None, STRENGTH_MAGNETOMETRIC) => messages::GM_SITE_RELIC.into(),
            _ => messages::GM_SITE_COMBAT.into(),
        };
        let kind = if s.kind == Kind::Anomaly { messages::GM_SITE_ANOMALY } else { messages::GM_SITE_SIGNATURE };
        lines.push(messages::gm_site_row(&s.target, kind, &what, &name.unwrap_or_default()));
    }
    lines.sort();
    Ok(lines.join("\n"))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "scanMgr" => scan_mgr::dispatch(c, call).await,
        "wormholeMgr" => wormhole_mgr::dispatch(c, call).await,
        _ => None,
    }
}

#[cfg(test)] mod tests;
