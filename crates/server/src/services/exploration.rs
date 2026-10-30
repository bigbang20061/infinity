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

fn class_from_security(security: f64) -> i64 {
    if security >= 0.45 {
        7
    } else if security > 0.0 {
        8
    } else {
        9
    }
}

async fn system_info(db: &Db, system: i64) -> Result<SystemInfo, CallError> {
    let r = db
        .query_one(
            "SELECT COALESCE(s.security, 0)::float8, s.faction_id::bigint, s.region_id::bigint,
                    (CASE WHEN s.solar_system_id < 31000000
                              AND COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int, 7) IN (7, 8, 9)
                         THEN CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END
                         ELSE COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int) END)::bigint
             FROM sde.map_solar_systems s
             JOIN sde.map_constellations c ON c.constellation_id = s.constellation_id
             JOIN sde.map_regions r ON r.region_id = s.region_id
             WHERE s.solar_system_id = $1",
            &[&(system as i32)],
        )
        .await
        .map_err(db_err)?;
    let security: f64 = r.get(0);
    Ok(SystemInfo {
        security,
        faction: r.get(1),
        region: r.get(2),
        class: r.get::<_, Option<i64>>(3).unwrap_or_else(|| class_from_security(security)),
    })
}

/// Systems a wormhole can lead to, by wormhole class: k-space (7-9) and the C1-C6
/// wormhole regions; no Jove space, Pochven, Thera or the Drifter systems.
async fn systems_of_class(db: &Db, class: i64) -> Result<Vec<i64>, CallError> {
    static CACHE: OnceLock<Mutex<Option<HashMap<i64, Vec<i64>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(map) = cache.lock().unwrap().as_ref() {
        return Ok(map.get(&class).cloned().unwrap_or_default());
    }
    let rows = db
        .query(
            "SELECT s.solar_system_id::bigint,
                    COALESCE(CASE WHEN s.solar_system_id < 31000000
                              AND COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int, 7) IN (7, 8, 9)
                         THEN CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END
                         ELSE COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int) END,
                             CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END)::bigint
             FROM sde.map_solar_systems s
             JOIN sde.map_constellations c ON c.constellation_id = s.constellation_id
             JOIN sde.map_regions r ON r.region_id = s.region_id
             WHERE s.solar_system_id < 31002605 AND s.region_id NOT IN (10000004, 10000017, 10000019)
               AND EXISTS (SELECT 1 FROM sde.map_celestials m WHERE m.solar_system_id = s.solar_system_id AND m.group_id = 7)",
            &[],
        )
        .await
        .map_err(db_err)?;
    let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
    for r in rows {
        map.entry(r.get(1)).or_default().push(r.get(0));
    }
    let out = map.get(&class).cloned().unwrap_or_default();
    *cache.lock().unwrap() = Some(map);
    Ok(out)
}

/// The pirates of an empire's space (or of a pirate-held system).
fn pirate_faction(faction: Option<i64>, region: i64) -> i64 {
    match faction {
        Some(FACTION_CALDARI) => FACTION_GURISTAS,                  // Caldari: Guristas
        Some(FACTION_MINMATAR) => FACTION_ANGEL_CARTEL,             // Minmatar: Angel Cartel
        Some(FACTION_AMARR) if region % 2 == 0 => 500019,           // Amarr: Sansha's Nation
        Some(FACTION_AMARR | 500007 | 500008) => 500012,            // Amarr, Ammatar, Khanid: Blood Raiders
        Some(FACTION_GALLENTE) => 500020,                           // Gallente: Serpentis
        Some(f @ (FACTION_GURISTAS | FACTION_ANGEL_CARTEL | 500012 | 500019 | 500020)) => f,
        _ => [FACTION_GURISTAS, FACTION_ANGEL_CARTEL, 500012, 500019, 500020][(region % 5) as usize],
    }
}


fn site_distance() -> f64 {
    SITE_OFFSET.0 + rand01() * (SITE_OFFSET.1 - SITE_OFFSET.0)
}

/// A random point well off the surface of a random planet of the system.
async fn exploration_site_position(db: &Db, system: i64) -> Result<V3, CallError> {
    let (base, radius) = super::site_position(db, system).await?.unwrap_or(([AU, 0.0, 0.0], 0.0));
    Ok(off_surface(base, radius, site_distance()))
}

/// Sites and mission sites are placed clear of planets' surfaces; ones made before that
/// (measured from the centre, so inside large planets) are moved out at startup, with
/// their scan overlay positions and any asteroids.
pub async fn unbury_sites(db: &Db) -> Result<(), CallError> {
    // (table, key column, has overlay/asteroids)
    for (table, key, exploration) in [("game.exploration_sites", "site_id", true), ("game.agent_missions", "(character_id, agent_id)", false)] {
        let rows = db
            .query(
                &format!(
                    "SELECT {key}::text, s.x, s.y, s.z, p.x, p.y, p.z, COALESCE(p.radius, 0)::float8 FROM {table} s
                     CROSS JOIN LATERAL (SELECT c.x, c.y, c.z, c.radius FROM sde.map_celestials c
                                         WHERE c.solar_system_id = s.system_id AND c.group_id = $1
                                         ORDER BY (c.x - s.x) ^ 2 + (c.y - s.y) ^ 2 + (c.z - s.z) ^ 2 LIMIT 1) p
                     WHERE sqrt((p.x - s.x) ^ 2 + (p.y - s.y) ^ 2 + (p.z - s.z) ^ 2) < COALESCE(p.radius, 0) + $2"
                ),
                &[&GROUP_PLANET, &SITE_REPAIR_CLEARANCE],
            )
            .await
            .map_err(db_err)?;
        for r in rows {
            let id: String = r.get(0);
            let old: V3 = [r.get(1), r.get(2), r.get(3)];
            let new = off_surface([r.get(4), r.get(5), r.get(6)], r.get(7), site_distance());
            let delta = [new[0] - old[0], new[1] - old[1], new[2] - old[2]];
            if exploration {
                let site: i64 = id.parse().unwrap_or(0);
                db
                    .execute(
                        "UPDATE game.exploration_sites SET x = x + $2, y = y + $3, z = z + $4,
                                overlay_x = overlay_x + $2, overlay_y = overlay_y + $3, overlay_z = overlay_z + $4 WHERE site_id = $1",
                        &[&site, &delta[0], &delta[1], &delta[2]],
                    )
                    .await
                    .map_err(db_err)?;
                db
                    .execute("UPDATE game.asteroids SET x = x + $2, y = y + $3, z = z + $4 WHERE field_id = $1", &[&site, &delta[0], &delta[1], &delta[2]])
                    .await
                    .map_err(db_err)?;
            } else {
                db
                    .execute(
                        "UPDATE game.agent_missions SET x = x + $2, y = y + $3, z = z + $4 WHERE (character_id, agent_id)::text = $1",
                        &[&id, &delta[0], &delta[1], &delta[2]],
                    )
                    .await
                    .map_err(db_err)?;
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

/// A combat dungeon of `faction` within the difficulty range: acceleration-gated
/// complexes are scanned down as signatures, open sites show as anomalies.
async fn pick_combat_dungeon(db: &Db, faction: i64, difficulty: (i64, i64), gated: bool, sleeper: bool) -> Result<Option<DungeonPick>, CallError> {
    let r = db
        .query_opt(
            "SELECT d.key::bigint, (d.data->>'dungeonNameID')::bigint, COALESCE((d.data->>'difficulty')::bigint, 1)
             FROM sde_native.dungeons d LEFT JOIN sde_native.localization_fsd_en_us l ON l.key = d.data->>'dungeonNameID'
             WHERE d.data->>'archetypeID' = $1 AND (d.data->>'factionID')::bigint = $2
               AND COALESCE((d.data->>'difficulty')::bigint, 1) BETWEEN $3 AND $4
               AND COALESCE(d.data->'connections' @? '$[*].allowedShipsList', false) = $5
               AND d.data ? 'dungeonNameID'
               AND (NOT $6 OR l.data->>'text' ~ '^(Unsecured )?(Perimeter|Frontier|Core|Outpost Frontier|Fortification Frontier|Sleeper)')
             ORDER BY random() LIMIT 1",
            &[&ARCHETYPE_COMBAT, &faction, &difficulty.0, &difficulty.1, &gated, &sleeper],
        )
        .await
        .map_err(db_err)?;
    Ok(r.map(|r| DungeonPick { id: r.get(0), name_id: r.get(1), difficulty: r.get(2) }))
}

fn new_site(system: i64, pos: V3, kind: Kind, strength_attr: i64, ease: f64, taken: &mut HashSet<String>) -> Site {
    let target = target_id(taken);
    taken.insert(target.clone());
    // Overlay deviation: 4 AU for strong signals, up to 8 AU for weak ones
    // (explorationscanner.signalTracker.CalculateDeviationForSignal).
    let deviation = 4.0 * AU + (1.0 - ease.clamp(0.0, 1.0)) * 4.0 * AU;
    let overlay_pos = if kind == Kind::Signature { offset(pos, rand01() * deviation) } else { pos };
    Site {
        id: NEXT_SITE_ID.fetch_add(1, Ordering::Relaxed),
        target,
        system,
        pos,
        kind,
        strength_attr,
        ease,
        dungeon_id: None,
        name_id: None,
        archetype: None,
        faction: None,
        difficulty: 1,
        overlay_pos,
        deviation: if kind == Kind::Signature { deviation } else { 0.0 },
        wormhole: None,
        npcs: None,
        in_space: false,
        containers: None,
        hacked: vec![],
    }
}

/// One combat site for the system: an anomaly or a (gated) signature.
async fn combat_site(db: &Db, system: i64, info: &SystemInfo, kind: Kind, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    let jspace = (1..=6).contains(&info.class);
    let (faction, range) = if jspace {
        let lo = (info.class + 1) / 2 + 1; // C1-2: 2, C3-4: 3, C5-6: 4
        (500021, (lo, lo + 1))
    } else {
        let range = match info.class {
            7 => (1, 2),
            8 => (2, 4),
            _ => (3, 5),
        };
        (pirate_faction(info.faction, info.region), range)
    };
    let gated = kind == Kind::Signature && !jspace;
    let Some(d) = pick_combat_dungeon(db, faction, range, gated, jspace).await? else { return Ok(None) };
    let pos = exploration_site_position(db, system).await?;
    let ease = [1.0, 0.8, 0.6, 0.5, 0.4][(d.difficulty.clamp(1, 5) - 1) as usize];
    let mut site = new_site(system, pos, kind, STRENGTH_ALL, ease, taken);
    site.dungeon_id = Some(d.id);
    site.name_id = d.name_id;
    site.archetype = Some(24);
    site.faction = Some(faction);
    site.difficulty = d.difficulty;
    Ok(Some(site))
}

/// A wormhole out of `system` (of `class`) to a class drawn from `targets`, with its K162
/// in the destination. Returns (wormhole site, K162 site).
async fn wormhole_pair(
    db: &Db,
    system: i64,
    class: i64,
    targets: &[i64],
    taken: &mut HashSet<String>,
) -> Result<Option<(Site, Site)>, CallError> {
    let types = db
        .query(
            "SELECT t.type_id, a.value::bigint FROM sde.inv_types t
             JOIN sde.type_attributes a ON a.type_id = t.type_id AND a.attribute_id = $1
             WHERE t.group_id = 988 AND t.type_id <> $2 AND a.value::bigint = ANY($3)",
            &[&ATTR_WH_TARGET_CLASS, &TYPE_K162, &targets],
        )
        .await
        .map_err(db_err)?;
    let mut types: Vec<(i32, i64)> = types.iter().map(|r| (r.get::<_, i32>(0), r.get::<_, i64>(1))).collect();
    for i in (1..types.len()).rev() {
        types.swap(i, (rand01() * (i + 1) as f64) as usize % (i + 1));
    }
    // The first type (in random order) whose class has somewhere to lead.
    let mut chosen = None;
    for (type_id, dest_class) in types {
        let candidates: Vec<i64> = systems_of_class(db, dest_class).await?.into_iter().filter(|s| *s != system).collect();
        if let Some(dest) = pick(&candidates) {
            chosen = Some((type_id, dest_class, dest));
            break;
        }
    }
    let Some((type_id, dest_class, dest_system)) = chosen else {
        warn!("exploration: no wormhole destination of class {targets:?} from {system}");
        return Ok(None);
    };
    let a = db
        .query_one(
            "SELECT COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2), 960),
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $3), 1000000000),
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $4), 300000000),
                    (SELECT (r.data->>'nebulaID')::bigint FROM sde.map_solar_systems s JOIN sde.map_regions r ON r.region_id = s.region_id
                     WHERE s.solar_system_id = $5),
                    (SELECT (r.data->>'nebulaID')::bigint FROM sde.map_solar_systems s JOIN sde.map_regions r ON r.region_id = s.region_id
                     WHERE s.solar_system_id = $6)",
            &[&type_id, &ATTR_WH_LIFE_MINUTES, &ATTR_WH_MASS, &ATTR_WH_JUMP_MASS, &(dest_system as i32), &(system as i32)],
        )
        .await
        .map_err(db_err)?;
    let (life, mass, jump): (f64, f64, f64) = (a.get(0), a.get(1), a.get(2));
    let born = filetime_now();
    let mut here = new_site(system, exploration_site_position(db, system).await?, Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, taken);
    // The far side's target IDs are drawn from its own system's set when it is generated;
    // a clash there is harmless (results are per system).
    let mut there = new_site(dest_system, exploration_site_position(db, dest_system).await?, Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, &mut HashSet::new());
    let base = Wormhole {
        type_id,
        dest_system,
        dest_class,
        other: there.id,
        born,
        life_minutes: life,
        mass_max: mass,
        mass_used: 0.0,
        jump_mass: jump,
        nebula: a.get::<_, Option<i64>>(3).unwrap_or(0),
    };
    there.wormhole = Some(Wormhole {
        type_id: TYPE_K162,
        dest_system: system,
        dest_class: class,
        other: here.id,
        nebula: a.get::<_, Option<i64>>(4).unwrap_or(0),
        ..base.clone()
    });
    here.wormhole = Some(base);
    Ok(Some((here, there)))
}

// ---------------------------------------------------------------- persistence

const SITE_COLUMNS: &str = "site_id, system_id, target, kind, strength_attr, ease, dungeon_id, name_id, archetype, faction, difficulty,
    x, y, z, overlay_x, overlay_y, overlay_z, deviation, in_space, containers, hacked,
    wh_type, wh_dest_system, wh_dest_class, wh_other, wh_born, wh_life_minutes, wh_mass_max, wh_mass_used, wh_jump_mass, wh_nebula";

fn site_from_row(r: &tokio_postgres::Row) -> Site {
    let wormhole = r.get::<_, Option<i32>>(21).map(|type_id| Wormhole {
        type_id,
        dest_system: r.get(22),
        dest_class: r.get(23),
        other: r.get(24),
        born: r.get(25),
        life_minutes: r.get(26),
        mass_max: r.get(27),
        mass_used: r.get(28),
        jump_mass: r.get(29),
        nebula: r.get::<_, Option<i64>>(30).unwrap_or(0),
    });
    Site {
        id: r.get(0),
        system: r.get(1),
        target: r.get(2),
        kind: if r.get::<_, i16>(3) == 0 { Kind::Anomaly } else { Kind::Signature },
        strength_attr: r.get(4),
        ease: r.get(5),
        dungeon_id: r.get(6),
        name_id: r.get(7),
        archetype: r.get(8),
        faction: r.get(9),
        difficulty: r.get(10),
        pos: [r.get(11), r.get(12), r.get(13)],
        overlay_pos: [r.get(14), r.get(15), r.get(16)],
        deviation: r.get(17),
        in_space: r.get(18),
        containers: r.get(19),
        hacked: r.get(20),
        wormhole,
        npcs: None,
    }
}

async fn save_site(db: &(impl Executor + ?Sized), s: &Site) -> Result<(), CallError> {
    let w = s.wormhole.as_ref();
    let kind: i16 = if s.kind == Kind::Anomaly { 0 } else { 1 };
    db
        .execute(
            &format!(
                "INSERT INTO game.exploration_sites ({SITE_COLUMNS})
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21,
                         $22, $23, $24, $25, $26, $27, $28, $29, $30, $31)
                 ON CONFLICT (site_id) DO UPDATE SET in_space = EXCLUDED.in_space, containers = EXCLUDED.containers,
                     hacked = EXCLUDED.hacked, wh_mass_used = EXCLUDED.wh_mass_used"
            ),
            &[
                &s.id,
                &s.system,
                &s.target,
                &kind,
                &s.strength_attr,
                &s.ease,
                &s.dungeon_id,
                &s.name_id,
                &s.archetype,
                &s.faction,
                &s.difficulty,
                &s.pos[0],
                &s.pos[1],
                &s.pos[2],
                &s.overlay_pos[0],
                &s.overlay_pos[1],
                &s.overlay_pos[2],
                &s.deviation,
                &s.in_space,
                &s.containers,
                &s.hacked,
                &w.map(|w| w.type_id),
                &w.map(|w| w.dest_system),
                &w.map(|w| w.dest_class),
                &w.map(|w| w.other),
                &w.map(|w| w.born),
                &w.map(|w| w.life_minutes),
                &w.map(|w| w.mass_max),
                &w.map(|w| w.mass_used),
                &w.map(|w| w.jump_mass),
                &w.map(|w| w.nebula),
            ],
        )
        .await
        .map_err(db_err)?;
    Ok(())
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
    db.execute("DELETE FROM game.exploration_sites WHERE site_id = $1", &[&site_id]).await.map_err(db_err)?;
    db.execute("DELETE FROM game.exploration_scanned WHERE site_id = $1", &[&site_id]).await.map_err(db_err)?;
    Ok(())
}

/// Has `char_id` scanned this signature down (this session or before)?
async fn is_scanned(db: &Db, char_id: i64, site_id: i64) -> Result<bool, CallError> {
    if state().scanned.contains(&(char_id, site_id)) {
        return Ok(true);
    }
    let found = db
        .query_opt("SELECT 1 FROM game.exploration_scanned WHERE character_id = $1 AND site_id = $2", &[&char_id, &site_id])
        .await
        .map_err(db_err)?
        .is_some();
    if found {
        state().scanned.insert((char_id, site_id));
    }
    Ok(found)
}

async fn mark_scanned(db: &Db, char_id: i64, site_id: i64) -> Result<(), CallError> {
    if !state().scanned.insert((char_id, site_id)) {
        return Ok(());
    }
    db
        .execute("INSERT INTO game.exploration_scanned (character_id, site_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&char_id, &site_id])
        .await
        .map_err(db_err)?;
    Ok(())
}

/// Serialises loading and generation (so two pilots entering a system generate it once).
fn load_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// Bring a system's sites into memory: load what is stored (closing wormholes past their
/// lifetime), generate the system's sites on its first visit ever, and put wormholes and
/// site containers that were in space back into space.
async fn ensure_loaded(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if state().loaded.contains(&system) {
        return Ok(());
    }
    let _guard = load_lock().lock().await;
    if state().loaded.contains(&system) {
        return Ok(());
    }
    if NEXT_SITE_ID.load(Ordering::Relaxed) == 0 {
        let next: i64 = db
            .query_one(
                "SELECT GREATEST(nextval('game.exploration_site_seq'), COALESCE((SELECT max(site_id) + 1 FROM game.exploration_sites), 0))",
                &[],
            )
            .await
            .map_err(db_err)?
            .get(0);
        NEXT_SITE_ID.store(next, Ordering::Relaxed);
    }
    // Wormholes past their lifetime close at both ends.
    db
        .execute(
            "DELETE FROM game.exploration_sites WHERE wh_born IS NOT NULL AND wh_born + (wh_life_minutes * 60 * 10000000)::bigint < $1",
            &[&filetime_now()],
        )
        .await
        .map_err(db_err)?;
    let rows = db
        .query(&format!("SELECT {SITE_COLUMNS} FROM game.exploration_sites WHERE system_id = $1"), &[&system])
        .await
        .map_err(db_err)?;
    let stored: Vec<Site> = rows.iter().map(site_from_row).collect();
    let generated = db
        .query_opt("SELECT 1 FROM game.exploration_systems WHERE system_id = $1", &[&system])
        .await
        .map_err(db_err)?
        .is_some();
    let new = if generated {
        // Systems generated before mining sites existed get theirs now.
        if stored.iter().any(|s| s.mining_field().is_some()) {
            vec![]
        } else {
            let info = system_info(db, system).await?;
            let mut taken = stored.iter().map(|s| s.target.clone()).collect();
            mining_sites(db, system, &info, &mut taken).await?
        }
    } else {
        let taken = stored.iter().map(|s| s.target.clone()).collect();
        generate(db, system, taken).await?
    };
    for s in &new {
        save_site(db, s).await?;
    }
    if !generated {
        db
            .execute("INSERT INTO game.exploration_systems (system_id, generated_at) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&system, &filetime_now()])
            .await
            .map_err(db_err)?;
    }
    {
        let mut st = state();
        for s in stored.iter().chain(new.iter()) {
            st.sites.entry(s.id).or_insert_with(|| s.clone());
        }
        st.loaded.insert(system);
    }
    // Back into space: wormholes someone had found, and spawned site containers.
    for s in &stored {
        if let (true, Some(w)) = (s.in_space, &s.wormhole) {
            world.add_ship(system, wormhole_ball(s, w)).await.map_err(CallError::Internal)?;
        }
        match (&s.containers, s.mining_field()) {
            // A mined field's rocks come back with the system's (services/mining); its
            // watch resumes.
            (Some(rocks), Some(_)) => {
                track_site_clearance(db.clone(), world.clone(), s.id, system, rocks.clone());
            }
            (Some(containers), None) => restore_containers(db, world, s, containers).await?,
            _ => {}
        }
    }
    Ok(())
}

/// Wormhole space always has a connection out to known space: if the system has none (it
/// collapsed, or was never made), a new one opens.
async fn ensure_static_exit(db: &Db, world: &World, system: i64) -> Result<(), CallError> {
    let has_exit = system_sites(system).iter().any(|s| s.wormhole.as_ref().is_some_and(|w| (7..=9).contains(&w.dest_class)));
    if has_exit {
        return Ok(());
    }
    let info = system_info(db, system).await?;
    if !(1..=6).contains(&info.class) {
        return Ok(());
    }
    let mut taken: HashSet<String> = system_sites(system).into_iter().map(|s| s.target).collect();
    let Some((here, there)) = wormhole_pair(db, system, info.class, &[7, 8, 9], &mut taken).await? else { return Ok(()) };
    save_site(db, &here).await?;
    save_site(db, &there).await?;
    info!("exploration: {system} gets a new exit {} -> {}", here.target, there.system);
    {
        let mut st = state();
        st.sites.insert(here.id, here.clone());
        st.sites.insert(there.id, there.clone());
    }
    broadcast_sites(world, system, std::slice::from_ref(&here), &[]).await;
    Ok(())
}

/// A system's sites, generated the first time anyone looks. Wormholes also create their
/// far end in the destination system.
async fn generate(db: &Db, system: i64, mut taken: HashSet<String>) -> Result<Vec<Site>, CallError> {
    let info = system_info(db, system).await?;
    let mut new = vec![];
    let jspace = (1..=6).contains(&info.class);
    let (anomalies, combat_sigs) = match info.class {
        1..=6 => (2 + info.class / 2, 0),
        7 => (2, 1),
        8 => (3, 1),
        _ => (4, 2),
    };
    for _ in 0..anomalies {
        new.extend(combat_site(db, system, &info, Kind::Anomaly, &mut taken).await?);
    }
    for _ in 0..combat_sigs {
        new.extend(combat_site(db, system, &info, Kind::Signature, &mut taken).await?);
    }
    new.extend(mining_sites(db, system, &info, &mut taken).await?);
    // A data and a relic site (two of each in null-sec).
    let hack_sites = if info.class == 9 { 2 } else { 1 };
    for _ in 0..hack_sites {
        for relic in [false, true] {
            new.extend(hack_site(db, system, &info, relic, &mut taken).await?);
        }
    }
    // Wormholes: a static out of wormhole space to known space or deeper, and in known
    // space a connection or two into wormhole space.
    let wormholes: Vec<&[i64]> = if jspace {
        let deeper: &[i64] = if info.class >= 4 { &[4, 5, 6] } else { &[1, 2, 3] };
        vec![&[7, 8, 9], deeper]
    } else {
        let into: &[i64] = &[1, 2, 3, 4, 5, 6];
        if info.class == 7 { vec![into] } else { vec![into, &[7, 8, 9]] }
    };
    for targets in wormholes {
        if let Some((here, there)) = wormhole_pair(db, system, info.class, targets, &mut taken).await? {
            new.push(here);
            new.push(there);
        }
    }
    info!("exploration: {system} (class {}, {:.2}) has {} new sites", info.class, info.security, new.len());
    Ok(new)
}

/// Ore anomalies everywhere, ice fields in some of known space, gas clouds to scan for.
async fn mining_sites(db: &Db, system: i64, info: &SystemInfo, taken: &mut HashSet<String>) -> Result<Vec<Site>, CallError> {
    use super::mining::Field;
    let jspace = (1..=6).contains(&info.class);
    let mut out = vec![];
    for _ in 0..if info.class == 9 { 2 } else { 1 } {
        out.extend(mining_site(db, system, info, Field::Ore, taken).await?);
    }
    if !jspace && rand01() < 0.3 {
        out.extend(mining_site(db, system, info, Field::Ice, taken).await?);
    }
    if jspace || info.class != 7 || rand01() < 0.5 {
        out.extend(mining_site(db, system, info, Field::Gas, taken).await?);
    }
    Ok(out)
}

/// An ore or ice anomaly, or a gas signature, named after one of the build's sites.
async fn mining_site(db: &Db, system: i64, info: &SystemInfo, field: super::mining::Field, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    use super::mining::Field;
    let jspace = (1..=6).contains(&info.class);
    let (archetype, pattern) = match field {
        Field::Ore => (ARCHETYPE_ORE, if jspace { "%Perimeter%" } else { "%Deposit%" }),
        Field::Ice => (ARCHETYPE_ICE, "%Belt%"),
        Field::Gas => (ARCHETYPE_GAS, if jspace { "%Reservoir%" } else { "% Nebula" }),
    };
    let r = db
        .query_opt(
            "SELECT d.key::bigint, (d.data->>'dungeonNameID')::bigint
             FROM sde_native.dungeons d JOIN sde_native.localization_fsd_en_us l ON l.key = d.data->>'dungeonNameID'
             WHERE d.data->>'archetypeID' = $1 AND l.data->>'text' LIKE $2 AND l.data->>'text' NOT LIKE '%OLD%'
             ORDER BY random() LIMIT 1",
            &[&archetype.to_string(), &pattern],
        )
        .await
        .map_err(db_err)?;
    let Some(r) = r else { return Ok(None) };
    let (kind, strength, ease) = match field {
        Field::Gas => (Kind::Signature, STRENGTH_LADAR, if jspace { 0.6 } else { 0.8 }),
        _ => (Kind::Anomaly, STRENGTH_GRAVIMETRIC, 1.0),
    };
    let mut site = new_site(system, exploration_site_position(db, system).await?, kind, strength, ease, taken);
    site.dungeon_id = Some(r.get(0));
    site.name_id = r.get(1);
    site.archetype = Some(archetype);
    site.difficulty = match info.class {
        7 => 1,
        8 => 2,
        _ => 3,
    };
    Ok(Some(site))
}

/// A data or relic site: the faction's (or the Sleepers') hackable ruins.
async fn hack_site(db: &Db, system: i64, info: &SystemInfo, relic: bool, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    let jspace = (1..=6).contains(&info.class);
    let faction = if jspace { 500021 } else { pirate_faction(info.faction, info.region) };
    // Retail naming by security: Local/Crumbling (high), Regional/Decayed (low),
    // Central/Ruined (null); Unsecured/Forgotten in wormhole space.
    let (prefix, tier, ease) = match (info.class, relic) {
        (1..=6, false) => ("Unsecured", (info.class + 1) / 2 + 1, 0.5),
        (1..=6, true) => ("Forgotten", (info.class + 1) / 2 + 1, 0.5),
        (7, false) => ("Local", 1, 0.8),
        (7, true) => ("Crumbling", 1, 0.8),
        (8, false) => ("Regional", 2, 0.6),
        (8, true) => ("Decayed", 2, 0.6),
        (_, false) => ("Central", 3, 0.5),
        (_, true) => ("Ruined", 3, 0.5),
    };
    let archetype = if relic { ARCHETYPE_RELIC } else { ARCHETYPE_DATA };
    let r = db
        .query_opt(
            "SELECT d.key::bigint, (d.data->>'dungeonNameID')::bigint, d.data->>'archetypeID' = $2
             FROM sde_native.dungeons d JOIN sde_native.localization_fsd_en_us l ON l.key = d.data->>'dungeonNameID'
             WHERE (d.data->>'factionID')::bigint = $1 AND d.data->>'archetypeID' IN ('24', '44', '45')
               AND l.data->>'text' LIKE $3 || ' %' AND l.data->>'text' NOT LIKE '%:%'
             ORDER BY d.data->>'archetypeID' = $2 DESC, random() LIMIT 1",
            &[&faction, &archetype, &prefix],
        )
        .await
        .map_err(db_err)?;
    let r = match r {
        Some(r) => Some(r),
        None => db
            .query_opt(
                "SELECT d.key::bigint, (d.data->>'dungeonNameID')::bigint, true FROM sde_native.dungeons d
                 WHERE d.data->>'archetypeID' = $1 AND (d.data->>'factionID')::bigint = $2 AND d.data ? 'dungeonNameID'
                 ORDER BY random() LIMIT 1",
                &[&archetype, &faction],
            )
            .await
            .map_err(db_err)?,
    };
    let Some(r) = r else { return Ok(None) };
    let strength = if relic { STRENGTH_MAGNETOMETRIC } else { STRENGTH_RADAR };
    let mut site = new_site(system, exploration_site_position(db, system).await?, Kind::Signature, strength, ease, taken);
    site.dungeon_id = Some(r.get(0));
    site.name_id = r.get(1);
    site.archetype = Some(if relic { 44 } else { 45 });
    site.faction = Some(faction);
    site.difficulty = tier.clamp(1, 4);
    Ok(Some(site))
}

// ---------------------------------------------------------------- the sensor overlay

fn anomaly_info(s: &Site) -> Value {
    keyval([
        ("position", pos_value(s.pos)),
        ("targetID", Value::str(&s.target)),
        ("difficulty", Value::Int(s.difficulty.max(1))),
        ("dungeonID", s.dungeon_id.map(Value::Int).unwrap_or(Value::None)),
        ("archetypeID", s.archetype.map(Value::Int).unwrap_or(Value::None)),
        ("siteID", Value::Int(s.id)),
        ("instanceID", Value::Int(s.id)),
        ("dungeonNameID", s.name_id.map(Value::Int).unwrap_or(Value::None)),
        ("factionID", s.faction.map(Value::Int).unwrap_or(Value::None)),
        ("scanStrengthAttribute", Value::Int(s.strength_attr)),
        ("allowedTypes", Value::list([])),
        ("entryObjectTypeID", Value::None),
        ("solarSystemID", Value::Int(s.system)),
    ])
}

fn signature_info(s: &Site) -> Value {
    keyval([
        ("position", pos_value(s.overlay_pos)),
        ("targetID", Value::str(&s.target)),
        ("difficulty", Value::Int(s.difficulty.max(1))),
        ("dungeonID", Value::None),
        ("archetypeID", Value::None),
        ("deviation", Value::Float(s.deviation)),
    ])
}

fn full_state(system: i64) -> Value {
    let sites = system_sites(system);
    let of = |kind: Kind, info: fn(&Site) -> Value| {
        Value::Dict(sites.iter().filter(|s| s.kind == kind).map(|s| (Value::Int(s.id), info(s))).collect())
    };
    Value::tuple([of(Kind::Anomaly, anomaly_info), of(Kind::Signature, signature_info), Value::Dict(vec![]), Value::Dict(vec![])])
}

/// Tell every pilot in `system` about sites that came or went.
async fn broadcast_sites(world: &World, system: i64, added: &[Site], removed: &[Site]) {
    let pilots: Vec<i64> = world.with(system, |s| s.pilots().filter_map(|b| b.char_id).collect()).await.unwrap_or_default();
    for (kind, event, info) in [
        (Kind::Anomaly, "OnSignalTrackerAnomalyUpdate", anomaly_info as fn(&Site) -> Value),
        (Kind::Signature, "OnSignalTrackerSignatureUpdate", signature_info),
    ] {
        let add: Vec<(Value, Value)> = added.iter().filter(|s| s.kind == kind).map(|s| (Value::Int(s.id), info(s))).collect();
        let gone: Vec<Value> = removed.iter().filter(|s| s.kind == kind).map(|s| Value::Int(s.id)).collect();
        if add.is_empty() && gone.is_empty() {
            continue;
        }
        for ch in &pilots {
            world.notify_pilot(*ch, event, vec![Value::Int(system), Value::Dict(add.clone()), Value::list(gone.clone())]);
        }
    }
}

// ---------------------------------------------------------------- probes

/// A pilot's probe strength (baseSensorStrength with the Astrometrics skills) and scan
/// time (ms, Astrometric Acquisition).
async fn probe_skills(db: &Db, char_id: i64) -> Result<(f64, f64), CallError> {
    let rows = db
        .query("SELECT type_id, level FROM game.skills WHERE character_id = $1 AND type_id = ANY($2)", &[&char_id, &vec![SKILL_ASTROMETRICS, SKILL_RANGEFINDING, SKILL_ACQUISITION]])
        .await
        .map_err(db_err)?;
    let level = |skill: i32| rows.iter().find(|r| r.get::<_, i32>(0) == skill).map(|r| r.get::<_, i32>(1) as f64).unwrap_or(0.0);
    let strength = (1.0 + 0.05 * level(SKILL_ASTROMETRICS)) * (1.0 + 0.05 * level(SKILL_RANGEFINDING));
    let scan_ms = 10_000.0 * (1.0 - 0.1 * level(SKILL_ACQUISITION));
    Ok((strength, scan_ms))
}

fn probe_value(id: i64, p: &Probe) -> Value {
    keyval([
        ("probeID", Value::Int(id)),
        ("typeID", Value::Int(p.type_id as i64)),
        ("pos", pos_value(p.pos)),
        ("destination", pos_value(p.pos)),
        ("expiry", Value::Int(p.expiry)),
    ])
}

/// dogmaIM.LaunchProbes(launcherID, count): probes leave the launcher's charge stack.
pub async fn launch_probes(c: &mut Client, call: &Call<'_>) -> CallResult {
    let module = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let wanted = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(1).clamp(1, MAX_PROBES as i64) as i32;
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Ok(Value::None);
    };
    let Some(flag) = c
        .db
        .query_opt("SELECT flag_id FROM game.items WHERE item_id = $1 AND location_id = $2", &[&module, &ship])
        .await
        .map_err(db_err)?
        .map(|r| r.get::<_, i32>(0))
    else {
        return Ok(Value::None);
    };
    let Some(loaded) = c
        .db
        .query_opt("SELECT type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2", &[&ship, &flag])
        .await
        .map_err(db_err)?
    else {
        return Err(CallError::User { msg: messages::MSG_NO_CHARGES.into(), dict: vec![] });
    };
    let (type_id, have): (i32, i32) = (loaded.get(0), loaded.get(1));
    let out = state().probes.values().filter(|p| p.char_id == char_id && p.system == system).count();
    let take = wanted.min(have).min((MAX_PROBES - out.min(MAX_PROBES)) as i32);
    if take <= 0 {
        return Ok(Value::None);
    }
    let attrs = c
        .db
        .query_one(
            "SELECT COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2), 40),
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $3), 3600000)",
            &[&type_id, &ATTR_BASE_SENSOR_STRENGTH, &ATTR_EXPLOSION_DELAY],
        )
        .await
        .map_err(db_err)?;
    let (base, lifetime_ms): (f64, f64) = (attrs.get(0), attrs.get(1));
    let (skill, _) = probe_skills(&c.db, char_id).await?;
    let pos = c.world.ball(system, ship).await.map(|b| b.pos).unwrap_or([0.0; 3]);
    let expiry = filetime_now() + (lifetime_ms * 10_000.0) as i64;
    enum Launch { Probes }
    let user_id = c.login.user_id;
    let db = c.db.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<Launch>| Box::pin(async move {
            let remaining = if take == have {
                tx.query_opt("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity = $4 RETURNING quantity", &[&ship, &flag, &type_id, &have]).await.map_err(db_err)?.map(|_| 0)
            } else {
                tx.query_opt("UPDATE game.module_charges SET quantity = quantity - $4 WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity >= $4 RETURNING quantity", &[&ship, &flag, &type_id, &take]).await.map_err(db_err)?.map(|r| r.get::<_, i32>(0))
            }.ok_or_else(|| super::notify_error(messages::INSUFFICIENT_PROBE_LAUNCHER_PROBES))?;
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

/// Field of a KeyVal (or dict) sent back by the client.
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Instance(_, state) => state.get(key),
        Value::Reduce { data, dict, .. } | Value::NewObj { data, dict, .. } => data
            .items()
            .and_then(|d| d.get(2))
            .and_then(|s| s.get(key))
            .or_else(|| dict.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == key)).map(|(_, v)| v)),
        other => other.get(key),
    }
}

fn as_pos(v: Option<&Value>) -> Option<V3> {
    let items = v?.items()?;
    match items {
        [x, y, z] => Some([x.as_float()?, y.as_float()?, z.as_float()?]),
        _ => None,
    }
}

/// How well a set of (position, range, strength) probes resolves a site.
fn certainty(site: &Site, probes: &[(V3, f64, f64)]) -> (f64, f64) {
    let mut signal = 0.0;
    let mut covering = 0;
    let mut tightest = f64::MAX;
    for &(pos, range, strength) in probes {
        let d = dist(pos, site.pos);
        if d > range {
            continue;
        }
        covering += 1;
        tightest = tightest.min(range);
        // Strength halves with each doubling of range beyond 0.25 AU, and fades toward
        // the edge of the probe's sphere.
        let falloff = 1.0 - (d / range).powi(2) * 0.5;
        signal += strength * (0.25 * AU / range).min(1.0) * falloff;
    }
    if covering == 0 {
        return (0.0, 0.0);
    }
    // One probe gives a sphere, two a circle, three a pair of points: only four or more
    // pin a site down.
    let cap = [0.0, 0.24, 0.49, 0.74, 1.0][covering.min(4)];
    let c = (signal * site.ease / CERTAINTY_DIVISOR).min(cap);
    (if c >= 0.999 { 1.0 } else { c }, tightest)
}

fn result_value(s: &Site, certainty: f64, spread: f64) -> Value {
    let anomaly = s.kind == Kind::Anomaly;
    let perfect = certainty >= 1.0;
    let mut f: Vec<(&str, Value)> = vec![
        ("id", Value::str(&s.target)),
        ("scanGroupID", Value::Int(if anomaly { SCAN_GROUP_ANOMALIES } else { SCAN_GROUP_SIGNATURES })),
        ("certainty", Value::Float(certainty)),
        ("itemID", Value::None),
    ];
    if perfect {
        f.push(("data", pos_value(s.pos)));
        f.push(("pos", pos_value(s.pos)));
    } else {
        // A sphere of uncertainty around a guess near the site.
        let radius = (spread * (1.0 - certainty)).max(10_000_000.0);
        f.push(("data", Value::Float(radius)));
        f.push(("pos", pos_value(offset(s.pos, rand01() * radius * 0.5))));
    }
    if certainty >= 0.25 {
        f.push(("groupID", Value::Int(if anomaly { GROUP_COSMIC_ANOMALY } else { GROUP_COSMIC_SIGNATURE })));
        f.push(("strengthAttributeID", Value::Int(s.strength_attr)));
        f.push(("difficulty", Value::Int(s.difficulty.max(1))));
    }
    if certainty >= 0.75 {
        f.push(("typeID", s.wormhole.as_ref().map(|w| Value::Int(w.type_id as i64)).unwrap_or(Value::None)));
        f.push(("dungeonID", s.dungeon_id.map(Value::Int).unwrap_or(Value::None)));
        f.push(("dungeonNameID", s.name_id.map(Value::Int).unwrap_or(Value::None)));
        f.push(("archetypeID", s.archetype.map(Value::Int).unwrap_or(Value::None)));
        f.push(("factionID", s.faction.map(Value::Int).unwrap_or(Value::None)));
    }
    keyval(f)
}

/// bound scanMgr.RequestScans(probes): move the probes to their destinations, scan, report.
async fn request_scans(c: &mut Client, call: &Call<'_>) -> CallResult {
    let (Some(system), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("charid")) else { return Ok(Value::None) };
    ensure_loaded(&c.db, &c.world, system).await?;
    let (_, scan_ms) = probe_skills(&c.db, char_id).await?;
    // (probeID, destination, range)
    let mut moved = vec![];
    if let Some(Value::Dict(probes)) = call.args.first() {
        for (k, p) in probes {
            let Some(id) = k.as_int() else { continue };
            let Some(dest) = as_pos(field(p, "destination")).or_else(|| as_pos(field(p, "pos"))) else { continue };
            let range = field(p, "scanRange").and_then(|v| v.as_float()).unwrap_or(8.0 * AU);
            moved.push((id, dest, range));
        }
    }
    let now = filetime_now();
    let mut scanning = vec![];
    let mut probe_ids = vec![];
    {
        let mut st = state();
        st.probes.retain(|_, p| p.expiry > now);
        for &(id, dest, range) in &moved {
            let Some(p) = st.probes.get_mut(&id).filter(|p| p.char_id == char_id && p.system == system) else { continue };
            p.pos = dest;
            scanning.push((dest, range, p.strength));
            probe_ids.push((id, probe_value(id, p)));
        }
    }
    let sent = Value::Dict(probe_ids.iter().map(|(id, v)| (Value::Int(*id), v.clone())).collect());
    c.notify("OnSystemScanStarted", vec![Value::Int(now), Value::Int(scan_ms as i64), sent]);
    let ids: Vec<i64> = probe_ids.iter().map(|(id, _)| *id).collect();
    let (world, db) = (c.world.clone(), c.db.clone());
    tokio::spawn(async move {
        if !world.after(system, scan_ms as u64).await {
            return;
        }
        let job_world = world.clone();
        world.post(system, move |s| s.defer(async move {
        let sites = system_sites(system);
        let mut results = vec![];
        let mut reported = HashSet::new();
        for s in &sites {
            // Anomalies and anything already scanned down are always exact; without
            // probes the ship's own scanner only feels signatures faintly.
            let known = s.kind == Kind::Anomaly || is_scanned(&db, char_id, s.id).await.unwrap_or(false);
            let (c, spread) = if known {
                (1.0, 0.0)
            } else if scanning.is_empty() {
                (0.01, s.deviation)
            } else {
                certainty(s, &scanning)
            };
            if c <= 0.0 {
                continue;
            }
            if c >= 1.0 && s.kind == Kind::Signature {
                if let Err(e) = mark_scanned(&db, char_id, s.id).await {
                    warn!("scan of {}: {}", s.target, err_text(e));
                }
            }
            reported.insert(s.target.clone());
            results.push(result_value(s, c, spread));
        }
        let absent: Vec<Value> = {
            let mut st = state();
            let before = st.reported.insert((char_id, system), reported.clone()).unwrap_or_default();
            before.difference(&reported).map(Value::str).collect()
        };
        job_world.notify_pilot(
            char_id,
            "OnSystemScanStopped",
            vec![Value::list(ids.into_iter().map(Value::Int)), Value::List(results), Value::List(absent)],
        );
        }));
    });
    Ok(Value::None)
}

/// bound scanMgr.RecoverProbes(probeIDs) -> recovered probe IDs; they return to cargo.
async fn recover_probes(c: &mut Client, call: &Call<'_>, destroy: bool) -> CallResult {
    let (Some(ship), Some(char_id)) = (c.session.int("shipid"), c.session.int("charid")) else { return Ok(Value::list([])) };
    let ids = value_ids(call.args.first());
    let mut back: HashMap<i32, i32> = HashMap::new();
    let mut done = vec![];
    let mut recovered_by_system: HashMap<i64, Vec<i64>> = HashMap::new();
    {
        let mut st = state();
        for id in ids {
            if st.probes.get(&id).is_some_and(|p| p.char_id == char_id) {
                let p = st.probes.remove(&id).unwrap();
                *back.entry(p.type_id).or_default() += 1;
                recovered_by_system.entry(p.system).or_default().push(id);
                done.push(id);
            }
        }
    }
    if !destroy {
        let mut outbox = Outbox::default();
        for (type_id, n) in back {
            super::combat::deposit(&c.db, &mut outbox, char_id, ship, FLAG_CARGO, type_id, n, ship).await?;
        }
        dispatch_client((), outbox, c)?;
    }
    for (system, gone) in recovered_by_system {
        if destroy {
            for id in gone {
                c.world.notify_pilot(char_id, "OnRemoveProbe", vec![Value::Int(id)]);
            }
        } else {
            c.world.schedule(system, PROBE_RECOVERY_DELAY_MS, move |s| {
                for id in gone {
                    s.notify_pilot(char_id, "OnRemoveProbe", vec![Value::Int(id)]);
                }
            });
        }
    }
    Ok(Value::list(done.into_iter().map(Value::Int)))
}

/// bound scanMgr.ConeScan(angle, range, x, y, z): everything in the cone, by type.
async fn cone_scan(c: &mut Client, call: &Call<'_>) -> CallResult {
    let n = |i: usize| call.args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    let (angle, range, dir) = (n(0), n(1), [n(2), n(3), n(4)]);
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(Value::list([])) };
    let Some(hits) = c.world.with(system, move |s| {
        let me = s.ball(ship)?;
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt().max(CONE_DIRECTION_MIN_LENGTH);
        let half = (angle / CONE_HALF_ANGLE_DIVISOR).min(std::f64::consts::PI);
        let mut hits = vec![];
        for b in s.balls() {
            if b.id == ship || b.cloak != 0 {
                continue;
            }
            let v = [b.pos[0] - me.pos[0], b.pos[1] - me.pos[1], b.pos[2] - me.pos[2]];
            let d = dist(b.pos, me.pos);
            if d > range || d < 1.0 {
                continue;
            }
            let cos = (v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2]) / (d * len);
            if half < std::f64::consts::PI && cos.clamp(-1.0, 1.0).acos() > half {
                continue;
            }
            hits.push((b.id, b.type_id));
        }
        Some(hits)
    }).await.flatten() else { return Ok(Value::list([])) };
    let types: Vec<i32> = hits.iter().map(|h| h.1).collect::<HashSet<_>>().into_iter().collect();
    let groups: HashMap<i32, i64> = c
        .db
        .query("SELECT type_id, group_id::bigint FROM sde.inv_types WHERE type_id = ANY($1)", &[&types])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| (r.get(0), r.get::<_, Option<i64>>(1).unwrap_or(0)))
        .collect();
    Ok(Value::List(
        hits.into_iter()
            .map(|(id, t)| keyval([("id", Value::Int(id)), ("typeID", Value::Int(t as i64)), ("groupID", Value::Int(groups.get(&t).copied().unwrap_or(0)))]))
            .collect(),
    ))
}

// ---------------------------------------------------------------- site content

/// NPC hull classes per site difficulty.
fn site_classes(difficulty: i64) -> &'static [&'static str] {
    match difficulty {
        ..=1 => &["Frigate", "Frigate", "Frigate", "Destroyer"],
        2 => &["Frigate", "Frigate", "Destroyer", "Cruiser", "Cruiser"],
        3 => &["Destroyer", "Cruiser", "Cruiser", "BattleCruiser", "BattleCruiser"],
        4 => &["Cruiser", "BattleCruiser", "BattleCruiser", "Battleship", "Battleship"],
        _ => &["BattleCruiser", "Battleship", "Battleship", "Battleship", "Battleship", "Cruiser"],
    }
}

/// NPC group name prefixes of a faction's site rats.
fn rat_prefix(faction: i64) -> &'static str {
    match faction {
        FACTION_GURISTAS => "Asteroid Guristas",
        FACTION_ANGEL_CARTEL => "Asteroid Angel Cartel",
        500012 => "Asteroid Blood Raiders",
        500019 => "Asteroid Sansha's Nation",
        500020 => "Asteroid Serpentis",
        _ => "Asteroid Rogue Drone",
    }
}

/// Sleeper NPC groups by wormhole class tier: (frigate, cruiser, battleship).
fn sleeper_groups(difficulty: i64) -> [&'static str; 3] {
    let tier = match difficulty {
        ..=2 => "Emergent",
        3..=4 => "Awakened",
        _ => "Sleepless",
    };
    match tier {
        "Emergent" => ["Deadspace Sleeper Emergent Sentinel", "Deadspace Sleeper Emergent Defender", "Deadspace Sleeper Emergent Patroller"],
        "Awakened" => ["Deadspace Sleeper Awakened Sentinel", "Deadspace Sleeper Awakened Defender", "Deadspace Sleeper Awakened Patroller"],
        _ => ["Deadspace Sleeper Sleepless Sentinel", "Deadspace Sleeper Sleepless Defender", "Deadspace Sleeper Sleepless Patroller"],
    }
}

/// Spawn a combat site's NPCs (once) and register its clearance callback.
async fn spawn_site_npcs(c: &mut Client, site: &Site) -> Result<(), CallError> {
    let faction = site.faction.unwrap_or(FACTION_GURISTAS);
    let sleepers = faction == 500021 && site.name_id.is_some() && site.archetype == Some(24) && site.strength_attr == STRENGTH_ALL && {
        let class = system_info(&c.db, site.system).await?.class;
        (1..=6).contains(&class)
    };
    let groups: Vec<String> = if sleepers {
        let [frig, cruiser, bs] = sleeper_groups(site.difficulty);
        site_classes(site.difficulty)
            .iter()
            .map(|class| match *class {
                "Frigate" | "Destroyer" => frig,
                "Cruiser" | "BattleCruiser" => cruiser,
                _ => bs,
            }.to_string())
            .collect()
    } else {
        site_classes(site.difficulty).iter().map(|class| format!("{} {class}", rat_prefix(faction))).collect()
    };
    let spawned = spawn_rats(c, site.system, site.pos, faction, &groups, (15_000.0, 40_000.0)).await?;
    info!(user = c.login.user_id, "site {} ({}) in {}: spawned {} NPCs", site.target, site.id, site.system, spawned.len());
    if let Some(s) = state().sites.get_mut(&site.id) {
        s.npcs = Some(spawned.clone());
    }
    track_site_clearance(c.db.clone(), c.world.clone(), site.id, site.system, spawned);
    Ok(())
}

/// Both combat sites and mined-out fields linger and replace after their group clears.
fn track_site_clearance(db: Db, world: Arc<World>, site_id: i64, system: i64, members: Vec<i64>) {
    let completion_world = world.clone();
    world.post(system, move |s| {
        s.spawn_group(members, move |s| {
            s.schedule(CLEARED_SITE_LINGER_MS, move |s| {
                s.defer(async move {
                    if let Err(e) = replace_site(&db, &completion_world, site_id).await {
                        warn!("replacing site {site_id}: {}", err_text(e));
                    }
                });
            });
        });
    });
}

/// Put one hostile NPC of each of `groups` (a random type of the group) in space for
/// `faction`, between `spread.0` and `spread.1` metres from `center`.
async fn spawn_rats(c: &mut Client, system: i64, center: V3, faction: i64, groups: &[String], spread: (f64, f64)) -> Result<Vec<i64>, CallError> {
    let corp: i64 = c
        .db
        .query_opt("SELECT corporation_id::bigint FROM sde.chr_factions WHERE faction_id::bigint = $1", &[&faction])
        .await
        .map_err(db_err)?
        .and_then(|r| r.get::<_, Option<i64>>(0))
        .unwrap_or(1000127);
    let ms = now_ms();
    let mut spawned = vec![];
    for group in groups {
        let Some(r) = c
            .db
            .query_opt(
                "SELECT t.type_id, COALESCE(t.radius, 50)::float8, COALESCE(t.mass, 1000000)::float8,
                        (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 37),
                        (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 70)
                 FROM sde.inv_types t JOIN sde.inv_groups g ON g.group_id = t.group_id
                 WHERE g.category_id = 11 AND g.name = $1 ORDER BY random() LIMIT 1",
                &[group],
            )
            .await
            .map_err(db_err)?
        else {
            continue;
        };
        let d = spread.0 + rand01() * (spread.1 - spread.0);
        let v = rand_dir();
        let ball = Ball {
            id: super::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
            type_id: r.get(0),
            owner_id: corp,
            corp_id: corp,
            alliance_id: -1,
            char_id: None,
            signature_radius: None,
            mode: MODE_STOP,
            radius: r.get::<_, f64>(1) as f32,
            mass: r.get(2),
            max_vel: r.get::<_, Option<f64>>(3).unwrap_or(300.0) as f32,
            agility: r.get::<_, Option<f64>>(4).unwrap_or(3.0) as f32,
            speed_fraction: 0.0,
            pos: [center[0] + v[0] * d, center[1] + v[1] * d * 0.3, center[2] + v[2] * d],
            vel: [0.0; 3],
            goto: [0.0; 3],
            follow_id: 0,
            follow_range: 0.0,
            free: true,
            global: false,
            massive: true,
            warp: None,
            warp_factor: 0,
            last_ms: ms,
            cloak: 0,
            cloak_until: 0,
            health: [1.0; 3],
            launcher: 0,
            slim: vec![("corpID", Value::Int(corp))],
        };
        let (id, type_id) = (ball.id, ball.type_id);
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
        super::combat::spawn_npc(c.db.clone(), c.world.clone(), system, id, type_id, Some(crate::space::HOSTILE_TO_ALL)).await?;
        spawned.push(id);
    }
    Ok(spawned)
}

// ---------------------------------------------------------------- belt rats

/// Existing server policy: a cleared belt stays empty before rats respawn.
const BELT_RAT_RESPAWN_MS: u64 = 900_000;

/// Belt rat hull classes by security: a few frigates in high-sec, up to battleships in null.
fn belt_classes(security: f64) -> &'static [&'static str] {
    if security >= 0.45 {
        &["Frigate", "Frigate", "Destroyer"]
    } else if security > 0.0 {
        &["Frigate", "Destroyer", "Cruiser", "Cruiser"]
    } else {
        &["Cruiser", "BattleCruiser", "BattleCruiser", "Battleship"]
    }
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

/// A finished site leaves (with any containers) and one of the same kind appears
/// elsewhere in the system.
async fn replace_site(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
    let Some(old) = state().sites.get(&site_id).cloned() else { return Ok(()) };
    let system = old.system;
    let info = system_info(db, system).await?;
    let mut taken: HashSet<String> = system_sites(system).into_iter().filter(|s| s.id != site_id).map(|s| s.target).collect();
    let new = if let Some(field) = old.mining_field() {
        mining_site(db, system, &info, field, &mut taken).await?
    } else if old.is_hack_site() {
        hack_site(db, system, &info, old.strength_attr == STRENGTH_MAGNETOMETRIC, &mut taken).await?
    } else {
        combat_site(db, system, &info, old.kind, &mut taken).await?
    };
    enum Replacement { Apply(Site, Option<Site>) }
    db.transaction(
        move |tx, outbox: &mut Outbox<Replacement>| {
            let (old, new) = (old.clone(), new.clone());
            Box::pin(async move {
                delete_site(tx, old.id).await?;
                for id in old.containers.iter().flatten() {
                    Executor::execute(tx, "DELETE FROM game.items WHERE location_id = $1 OR item_id = $1", &[id]).await.map_err(db_err)?;
                }
                if let Some(site) = &new {
                    save_site(tx, site).await?;
                }
                outbox.operation(Replacement::Apply(old, new));
                Ok(())
            })
        },
        |(), outbox: Outbox<Replacement>| async move {
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(world, None)?,
                    Effect::Operation(Replacement::Apply(old, new)) => {
                        info!("site {} in {system} done", old.target);
                        state().sites.remove(&old.id);
                        for id in old.containers.iter().flatten() {
                            world.remove_ball(system, *id).await;
                        }
                        if let Some(site) = &new {
                            state().sites.insert(site.id, site.clone());
                        }
                        broadcast_sites(world, system, new.as_slice(), std::slice::from_ref(&old)).await;
                    }
                }
            }
            Ok(())
        },
    ).await
}

/// The wormhole age bucket (slimItem.wormholeAge: 0 new ... 3 about to close).
fn wormhole_age(w: &Wormhole) -> i64 {
    let left = 1.0 - (filetime_now() - w.born) as f64 / (minutes_to_seconds(w.life_minutes) * FILETIME_SECOND as f64);
    match left {
        l if l > 0.5 => 0,
        l if l > 0.25 => 1,
        l if l > 0.05 => 2,
        _ => 3,
    }
}

/// slimItem.wormholeSize: 1.0 fresh, 0.7 reduced, 0.4 critical.
fn wormhole_size(w: &Wormhole) -> f64 {
    match 1.0 - w.mass_used / w.mass_max.max(1.0) {
        l if l > 0.5 => 1.0,
        l if l > 0.1 => 0.7,
        _ => 0.4,
    }
}

/// appConst.WH_SLIM_MAX_SHIP_MASS_*
fn jump_mass_class(jump_mass: f64) -> i64 {
    match jump_mass {
        m if m <= 5_000_000.0 => 1,
        m if m <= 62_000_000.0 => 2,
        m if m <= 1_000_000_000.0 => 3,
        _ => 4,
    }
}

fn wormhole_ball(s: &Site, w: &Wormhole) -> Ball {
    Ball {
        id: s.id,
        type_id: w.type_id,
        owner_id: 1,
        corp_id: 1,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: MODE_RIGID,
        radius: 3000.0,
        mass: 1e34,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos: s.pos,
        vel: [0.0; 3],
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: false,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![
            ("wormholeSize", Value::Float(wormhole_size(w))),
            ("wormholeAge", Value::Int(wormhole_age(w))),
            ("maxShipJumpMass", Value::Int(jump_mass_class(w.jump_mass))),
            ("otherSolarSystemClass", Value::Int(w.dest_class)),
            ("nebulaType", Value::Int(w.nebula)),
            ("isDestTriglavian", Value::Bool(false)),
        ],
    }
}

/// Put a wormhole site's wormhole in space (once).
async fn ensure_wormhole_ball(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
    let Some((site, w)) = ({
        let mut st = state();
        st.sites.get_mut(&site_id).filter(|s| !s.in_space).and_then(|s| {
            s.in_space = true;
            s.wormhole.clone().map(|w| (s.clone(), w))
        })
    }) else {
        return Ok(());
    };
    world.add_ship(site.system, wormhole_ball(&site, &w)).await.map_err(CallError::Internal)?;
    persist(db, site_id).await
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

fn connection(site: i64) -> Option<i64> {
    state().sites.get(&site).and_then(|s| s.wormhole.as_ref().map(|w| site.min(w.other)))
}

/// wormholeMgr.GetWormholePolarization(wormholeID) -> (endTime, duration) while the pilot
/// can't jump this connection again (spacecomponents jumpPolarization), else None.
fn polarization(char_id: i64, site: i64) -> Option<(i64, i64)> {
    let connection = connection(site)?;
    let until = *state().polarized.get(&(char_id, connection))?;
    (until > filetime_now()).then_some((until, POLARIZATION_SECS * FILETIME_SECOND))
}

/// wormholeMgr.WormholeJump(wormholeID): through to the other side, next to its K162.
async fn wormhole_jump(c: &mut Client, wormhole_id: i64) -> CallResult {
    let (Some(system), Some(ship), Some(char_id)) = (c.session.int("solarsystemid"), c.session.int("shipid"), c.session.int("charid")) else {
        return Ok(Value::None);
    };
    let Some((site, w)) = state().sites.get(&wormhole_id).and_then(|s| s.wormhole.clone().map(|w| (s.clone(), w))) else {
        return Err(notify_error(messages::WORMHOLE_COLLAPSED));
    };
    let Some((gap, mass)) = c.world.with(system, move |s| {
        let (me, hole) = (s.ball(ship)?, s.ball(wormhole_id)?);
        Some((dist(me.pos, hole.pos) - hole.radius as f64 - me.radius as f64, me.mass))
    }).await.flatten() else { return Ok(Value::None) };
    if gap > WORMHOLE_JUMP_RANGE {
        return Err(CallError::User { msg: messages::MSG_NOT_CLOSE_ENOUGH_TO_JUMP.into(), dict: vec![("maxdist".into(), Value::Int(WORMHOLE_JUMP_RANGE as i64))] });
    }
    if polarization(char_id, wormhole_id).is_some() {
        return Err(notify_error(messages::WORMHOLE_POLARIZED_FOR_SHIP));
    }
    if mass > w.jump_mass {
        return Err(notify_error(messages::wormhole_mass_limit(kg_to_tonnes(w.jump_mass))));
    }
    // The far side: its K162 (or the hole this K162 answers) goes into space now.
    ensure_loaded(&c.db, &c.world, w.dest_system).await?;
    ensure_wormhole_ball(&c.db, &c.world, w.other).await?;
    let Some(exit) = state().sites.get(&w.other).map(|s| s.pos) else { return Err(notify_error(messages::WORMHOLE_COLLAPSED)) };
    let pos = offset(exit, 4000.0 + rand01() * 2000.0);
    let jump = super::ship::Jump { through: wormhole_id, guid: "effects.JumpOutWormhole", other_type: Some(w.dest_class), dest_system: w.dest_system, pos };
    super::ship::jump_out_and_arrive(c, system, ship, char_id, jump).await?;
    info!(user = c.login.user_id, "ship {ship} jumped wormhole {} ({system} -> {})", site.target, w.dest_system);
    state().polarized.insert((char_id, site.id.min(w.other)), filetime_now() + POLARIZATION_SECS * FILETIME_SECOND);
    // Both ends share the mass budget; past it the connection collapses.
    let collapsed = {
        let mut st = state();
        let mut used = 0.0;
        for id in [site.id, w.other] {
            if let Some(wh) = st.sites.get_mut(&id).and_then(|s| s.wormhole.as_mut()) {
                wh.mass_used += mass;
                used = wh.mass_used;
            }
        }
        used >= w.mass_max
    };
    let world = c.world.clone();
    if collapsed {
        collapse(&c.db, &world, site.id).await?;
    } else {
        // Refresh the size the client shows for both ends.
        for id in [site.id, w.other] {
            persist(&c.db, id).await?;
            let Some((s, wh)) = state().sites.get(&id).and_then(|s| s.wormhole.clone().map(|w| (s.clone(), w))) else { continue };
            if s.in_space {
                world.remove_ball(s.system, s.id).await;
                let _ = world.add_ship(s.system, wormhole_ball(&s, &wh)).await;
            }
        }
    }
    Ok(Value::None)
}

/// A wormhole connection closes: both ends leave space and the scanners.
async fn collapse(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
    let (ids, sites) = {
        let st = state();
        let other = st.sites.get(&site_id).and_then(|s| s.wormhole.as_ref().map(|w| w.other));
        let mut ids: Vec<_> = std::iter::once(site_id).chain(other).collect();
        ids.sort_unstable();
        ids.dedup();
        let sites: Vec<_> = ids.iter().filter_map(|id| st.sites.get(id).cloned()).collect();
        (ids, sites)
    };
    enum Collapse { Remove(Vec<Site>) }
    db.transaction(
        move |tx, outbox: &mut Outbox<Collapse>| {
            let (ids, sites) = (ids.clone(), sites.clone());
            Box::pin(async move {
                for id in ids {
                    delete_site(tx, id).await?;
                }
                outbox.operation(Collapse::Remove(sites));
                Ok(())
            })
        },
        |(), outbox: Outbox<Collapse>| async move {
            for effect in outbox.into_entries() {
                match effect {
                    Effect::Notification(notification) => notification.dispatch(world, None)?,
                    Effect::Operation(Collapse::Remove(sites)) => for s in sites {
                        state().sites.remove(&s.id);
                        info!("wormhole {} in {} collapsed", s.target, s.system);
                        if s.in_space {
                            world.destroy_ball(s.system, s.id).await;
                        }
                        broadcast_sites(world, s.system, &[], &[s]).await;
                    },
                }
            }
            Ok(())
        },
    ).await
}

/// Close wormholes past their lifetime (checked whenever a pilot registers).
async fn expire_wormholes(db: &Db, world: &World) -> Result<(), CallError> {
    let now = filetime_now();
    let dead: Vec<i64> = state()
        .sites
        .values()
        .filter(|s| s.wormhole.as_ref().is_some_and(|w| now - w.born > filetime_ticks_from_seconds(minutes_to_seconds(w.life_minutes) as i64)))
        .map(|s| s.id)
        .collect();
    for id in dead {
        collapse(db, world, id).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- hackable containers

/// Container name prefixes of a faction's data and relic sites.
fn container_prefix(faction: i64) -> &'static str {
    match faction {
        FACTION_GURISTAS => "Guristas",
        FACTION_ANGEL_CARTEL => "Angel",
        500012 => "Blood",
        500019 => "Sansha",
        500020 => "Serpentis",
        _ => "Sleeper",
    }
}

/// Loot groups: datacores and decryptors from data sites, salvage from relic sites
/// (Sleeper components and ancient salvage in wormhole space).
fn loot_groups(relic: bool, sleeper: bool) -> Vec<i32> {
    match (relic, sleeper) {
        (false, false) => vec![333, 728, 729, 730, 731, 733, 734, 735, 979, 1304],
        (false, true) => vec![880, 732, 333],
        (true, false) => vec![754],
        (true, true) => vec![966, 964],
    }
}

fn container_ball(id: i64, type_id: i32, pos: V3, radius: f32, security_state: i64) -> Ball {
    Ball {
        id,
        type_id,
        owner_id: 1,
        corp_id: 1,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: MODE_RIGID,
        radius,
        mass: 1e34,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        // hackingcommon.hackingConstants.hackingState*: secure, being hacked, hacked
        slim: vec![("hackingSecurityState", Value::Int(security_state))],
    }
}

/// Spawn a data or relic site's containers with their loot (once).
async fn spawn_containers(c: &mut Client, site: &Site) -> Result<(), CallError> {
    let relic = site.strength_attr == STRENGTH_MAGNETOMETRIC;
    let faction = site.faction.unwrap_or(FACTION_GURISTAS);
    let prefix = container_prefix(faction);
    let sleeper = prefix == "Sleeper";
    let skill = if relic { SKILL_ARCHAEOLOGY } else { SKILL_HACKING };
    let types = c
        .db
        .query(
            "SELECT t.type_id, COALESCE(t.radius, 300)::float8 FROM sde.inv_types t
             JOIN sde.type_attributes h ON h.type_id = t.type_id
             JOIN sde.dogma_attributes hd ON hd.attribute_id = h.attribute_id AND hd.name = 'hackable'
             JOIN sde.type_attributes sk ON sk.type_id = t.type_id AND sk.attribute_id = $6 AND sk.value::int = $2
             LEFT JOIN sde.type_attributes tier ON tier.type_id = t.type_id
                  AND tier.attribute_id = (SELECT attribute_id FROM sde.dogma_attributes WHERE name = 'tierDifficulty')
             WHERE t.group_id = $1 AND (t.name LIKE $3 || ' %' OR ($4 AND t.name LIKE '%Sleeper%'))
               AND COALESCE(tier.value, 1) <= $5",
            &[&GROUP_SCATTER_CONTAINER, &skill, &prefix, &sleeper, &((site.difficulty + 1) as f64), &ATTR_REQUIRED_SKILL_1],
        )
        .await
        .map_err(db_err)?;
    let types: Vec<(i32, f64)> = types.iter().map(|r| (r.get(0), r.get(1))).collect();
    let count = 3 + (rand01() * 3.0) as usize;
    let value = CONTAINER_LOOT_VALUE[(site.difficulty.clamp(1, 4) - 1) as usize];
    let groups = loot_groups(relic, sleeper);
    let mut ids = vec![];
    for _ in 0..count {
        let Some((type_id, radius)) = pick(&types) else { break };
        let d = 2_000.0 + rand01() * 18_000.0;
        let v = rand_dir();
        let pos = [site.pos[0] + v[0] * d, site.pos[1] + v[1] * d * 0.3, site.pos[2] + v[2] * d];
        let id: i64 = c
            .db
            .query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z)
                 VALUES ($1, 1, $2, 0, 1, true, $3, $4, $5) RETURNING item_id",
                &[&type_id, &site.system, &pos[0], &pos[1], &pos[2]],
            )
            .await
            .map_err(db_err)?
            .get(0);
        let stacks = 2 + (rand01() * 2.0) as i64;
        let loot = c
            .db
            .query(
                "SELECT t.type_id, r.average FROM sde.inv_types t JOIN game.retail_prices r USING (type_id)
                 WHERE t.published AND t.group_id = ANY($1) AND r.average BETWEEN 1000 AND $2
                 ORDER BY random() LIMIT $3",
                &[&groups, &value, &stacks],
            )
            .await
            .map_err(db_err)?;
        for r in &loot {
            let (loot_type, price): (i32, f64) = (r.get(0), r.get(1));
            let quantity = ((value / stacks as f64 / price.max(1.0)) * (0.5 + rand01())).round().clamp(1.0, 500.0) as i32;
            c.db
                .execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, 1, $2, 0, $3)", &[&loot_type, &id, &quantity])
                .await
                .map_err(db_err)?;
        }
        c.world.add_ship(site.system, container_ball(id, type_id, pos, radius as f32, 0)).await.map_err(CallError::Internal)?;
        ids.push(id);
    }
    info!(user = c.login.user_id, "site {} ({}) in {}: {} containers", site.target, site.id, site.system, ids.len());
    if let Some(s) = state().sites.get_mut(&site.id) {
        s.containers = Some(ids);
    }
    persist(&c.db, site.id).await
}

/// Put a stored site's containers back in space after a restart.
async fn restore_containers(db: &Db, world: &World, site: &Site, ids: &[i64]) -> Result<(), CallError> {
    let rows = db
        .query(
            "SELECT i.item_id, i.type_id, i.x, i.y, i.z, COALESCE(t.radius, 300)::float8 FROM game.items i JOIN sde.inv_types t USING (type_id)
             WHERE i.item_id = ANY($1) AND i.x IS NOT NULL",
            &[&ids],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let id: i64 = r.get(0);
        let security = if site.hacked.contains(&id) { 2 } else { 0 };
        let ball = container_ball(id, r.get(1), [r.get(2), r.get(3), r.get(4)], r.get::<_, f64>(5) as f32, security);
        world.add_ship(site.system, ball).await.map_err(CallError::Internal)?;
    }
    Ok(())
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
                Executor::execute(tx, "UPDATE game.items SET owner_id = $2 WHERE location_id = $1", &[&container, &char_id]).await.map_err(db_err)?;
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
            let r = c
                .db
                .query_opt("SELECT site_id, system_id FROM game.exploration_sites WHERE wh_type IS NOT NULL ORDER BY random() LIMIT 1", &[])
                .await
                .map_err(db_err)?
                .ok_or_else(|| gm_error(messages::GM_NO_WORMHOLES))?;
            let (id, other_system): (i64, i64) = (r.get(0), r.get(1));
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
                .query_opt("SELECT data->>'text' FROM sde_native.localization_fsd_zh WHERE key = $1::text", &[&id.to_string()])
                .await
                .map_err(db_err)?
                .and_then(|r| r.get(0)),
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
    match (call.service, call.method) {
        ("scanMgr", "SignalTrackerRegister") => {
            let system = c.session.int("solarsystemid")?;
            if let Err(e) = super::mining::ensure_fields(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            if let Err(e) = expire_wormholes(&c.db, &c.world).await {
                return Some(Err(e));
            }
            if let Err(e) = ensure_loaded(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            if let Err(e) = ensure_static_exit(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            c.notify("OnSignalTrackerFullState", vec![Value::Int(system), full_state(system)]);
            Some(Ok(Value::None))
        }
        // Docked (the Agency refreshing local anomalies), it's the system the station is in.
        ("scanMgr", "GetFullState") => {
            let system = c.session.int("solarsystemid").or_else(|| c.session.int("solarsystemid2"))?;
            if let Err(e) = ensure_loaded(&c.db, &c.world, system).await {
                return Some(Err(e));
            }
            Some(Ok(full_state(system)))
        }
        ("scanMgr", "RequestScans") => Some(request_scans(c, call).await),
        ("scanMgr", "ConeScan") => Some(cone_scan(c, call).await),
        ("scanMgr", "RecoverProbes") => Some(recover_probes(c, call, false).await),
        ("scanMgr", "DestroyProbe") => Some(recover_probes(c, call, true).await.map(|_| Value::None)),
        ("scanMgr", "ReconnectToLostProbes") => {
            let (system, char_id) = (c.session.int("solarsystemid")?, c.session.int("charid")?);
            let probes: Vec<Value> = state().probes.iter().filter(|(_, p)| p.char_id == char_id && p.system == system).map(|(id, p)| probe_value(*id, p)).collect();
            for p in probes {
                c.notify("OnNewProbe", vec![p]);
            }
            Some(Ok(Value::None))
        }
        ("scanMgr", "SetActivityState" | "QAOverrideProbeExpiry") => Some(Ok(Value::None)),
        ("wormholeMgr", "GetWormholePolarization") => {
            let (char_id, id) = (c.session.int("charid")?, call.args.first().and_then(|v| v.as_int()).unwrap_or(0));
            Some(Ok(polarization(char_id, id).map(|(end, duration)| Value::tuple([Value::Int(end), Value::Int(duration)])).unwrap_or(Value::None)))
        }
        ("wormholeMgr", "WormholeJump") => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            Some(wormhole_jump(c, id).await)
        }
        _ => None,
    }
}

#[cfg(test)] mod tests;
