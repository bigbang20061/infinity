//! Generates exploration sites and wormhole pairs using server selection and placement
//! policy. The shared SDE supplies placement; runtime database `sde` map/type tables and
//! `sde_native.dungeons` and localization supply candidates, and generated sites are
//! persisted as game state. `eve/client/script/parklife/scanSvc.py:253-260` consumes
//! scan results, not a position generator. No usable general live-server dungeon-layout
//! source was validated in the decoded client tree; full layouts are not reproduced.

use super::*;

struct SystemRow { security: f64, faction: Option<i64>, region: i64, class: Option<i64> }
impl SystemRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { security: r.get("security"), faction: r.get("faction"), region: r.get("region"), class: r.get("class") } } }
struct ClassRow { system: i64, class: i64 }
impl ClassRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { system: r.get("system"), class: r.get("class") } } }
struct DungeonRow { id: i64, name_id: Option<i64> }
impl DungeonRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id: r.get("id"), name_id: r.get("name_id") } } }
struct HackDungeonRow { id: i64, name_id: Option<i64>, _preferred: Option<bool> }
impl HackDungeonRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id: r.get("id"), name_id: r.get("name_id"), _preferred: r.get("preferred") } } }
struct WormholeTypeRow { type_id: i32, class: i64 }
impl WormholeTypeRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), class: r.get("class") } } }
struct WormholeAttributesRow { life: f64, mass: f64, jump: f64, destination_nebula: Option<i64>, origin_nebula: Option<i64> }
impl WormholeAttributesRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { life: r.get("life"), mass: r.get("mass"), jump: r.get("jump"), destination_nebula: r.get("destination_nebula"), origin_nebula: r.get("origin_nebula") } } }

pub(super) fn class_from_security(security: f64) -> i64 {
    if security >= 0.45 {
        7
    } else if security > 0.0 {
        8
    } else {
        9
    }
}

pub(super) async fn system_info(db: &Db, system: i64) -> Result<SystemInfo, CallError> {
    let r = db
        .query_one(
            "SELECT COALESCE(s.security, 0)::float8 AS security, s.faction_id::bigint AS faction, s.region_id::bigint AS region,
                    (CASE WHEN s.solar_system_id < 31000000
                              AND COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int, 7) IN (7, 8, 9)
                         THEN CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END
                         ELSE COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int) END)::bigint AS class
             FROM sde.map_solar_systems s
             JOIN sde.map_constellations c ON c.constellation_id = s.constellation_id
             JOIN sde.map_regions r ON r.region_id = s.region_id
             WHERE s.solar_system_id = $1",
            &[&(system as i32)],
        )
        .await
        .map_err(db_err)?;
    let r = SystemRow::from_row(&r);
    Ok(SystemInfo { security: r.security, faction: r.faction, region: r.region,
        class: r.class.unwrap_or_else(|| class_from_security(r.security)) })
}

/// Systems a wormhole can lead to, by wormhole class: k-space (7-9) and the C1-C6
/// wormhole regions; no Jove space, Pochven, Thera or the Drifter systems.
pub(super) async fn systems_of_class(db: &Db, class: i64) -> Result<Vec<i64>, CallError> {
    static CACHE: OnceLock<Mutex<Option<HashMap<i64, Vec<i64>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(map) = cache.lock().unwrap().as_ref() {
        return Ok(map.get(&class).cloned().unwrap_or_default());
    }
    let rows = db
        .query(
            "SELECT s.solar_system_id::bigint AS system,
                    COALESCE(CASE WHEN s.solar_system_id < 31000000
                              AND COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int, 7) IN (7, 8, 9)
                         THEN CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END
                         ELSE COALESCE(c.wormhole_class_id, (r.data->>'wormholeClassID')::int) END,
                             CASE WHEN s.security >= 0.45 THEN 7 WHEN s.security > 0 THEN 8 ELSE 9 END)::bigint AS class
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
        let r = ClassRow::from_row(&r);
        map.entry(r.class).or_default().push(r.system);
    }
    let out = map.get(&class).cloned().unwrap_or_default();
    *cache.lock().unwrap() = Some(map);
    Ok(out)
}

/// The pirates of an empire's space (or of a pirate-held system).
pub(super) fn pirate_faction(faction: Option<i64>, region: i64) -> i64 {
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


pub(super) fn site_distance() -> f64 {
    SITE_OFFSET.0 + rand01() * (SITE_OFFSET.1 - SITE_OFFSET.0)
}

/// A random point well off the surface of a random planet of the system.
pub(super) fn exploration_site_position(sde: &Sde, system: i64) -> V3 {
    let (base, radius) = crate::services::site_position(sde, system).unwrap_or(([AU, 0.0, 0.0], 0.0));
    off_surface(base, radius, site_distance())
}
/// A combat dungeon of `faction` within the difficulty range: acceleration-gated
/// complexes are scanned down as signatures, open sites show as anomalies.
pub(super) async fn pick_combat_dungeon(db: &Db, faction: i64, difficulty: (i64, i64), gated: bool, sleeper: bool) -> Result<Option<DungeonPick>, CallError> {
    let r = db
        .query_opt(
            "SELECT d.key::bigint AS id, (d.data->>'dungeonNameID')::bigint AS name_id, COALESCE((d.data->>'difficulty')::bigint, 1) AS difficulty
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
    Ok(r.map(|r| DungeonPick::from_row(&r)))
}
pub(super) fn new_site(system: i64, pos: V3, kind: Kind, strength_attr: i64, ease: f64, taken: &mut HashSet<String>) -> Site {
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
pub(super) async fn combat_site(db: &Db, sde: &Sde, system: i64, info: &SystemInfo, kind: Kind, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
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
    let pos = exploration_site_position(sde, system);
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
pub(super) async fn wormhole_pair(
    db: &Db,
    sde: &Sde,
    system: i64,
    class: i64,
    targets: &[i64],
    taken: &mut HashSet<String>,
) -> Result<Option<(Site, Site)>, CallError> {
    let types = db
        .query(
            "SELECT t.type_id, a.value::bigint AS class FROM sde.inv_types t
             JOIN sde.type_attributes a ON a.type_id = t.type_id AND a.attribute_id = $1
             WHERE t.group_id = 988 AND t.type_id <> $2 AND a.value::bigint = ANY($3)",
            &[&ATTR_WH_TARGET_CLASS, &TYPE_K162, &targets],
        )
        .await
        .map_err(db_err)?;
    let mut types: Vec<(i32, i64)> = types.iter().map(|r| { let r = WormholeTypeRow::from_row(r); (r.type_id, r.class) }).collect();
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
            "SELECT COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2), 960) AS life,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $3), 1000000000) AS mass,
                    COALESCE((SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $4), 300000000) AS jump,
                    (SELECT (r.data->>'nebulaID')::bigint FROM sde.map_solar_systems s JOIN sde.map_regions r ON r.region_id = s.region_id
                     WHERE s.solar_system_id = $5) AS destination_nebula,
                    (SELECT (r.data->>'nebulaID')::bigint FROM sde.map_solar_systems s JOIN sde.map_regions r ON r.region_id = s.region_id
                     WHERE s.solar_system_id = $6) AS origin_nebula",
            &[&type_id, &ATTR_WH_LIFE_MINUTES, &ATTR_WH_MASS, &ATTR_WH_JUMP_MASS, &(dest_system as i32), &(system as i32)],
        )
        .await
        .map_err(db_err)?;
    let a = WormholeAttributesRow::from_row(&a);
    let (life, mass, jump) = (a.life, a.mass, a.jump);
    let born = filetime_now();
    let mut here = new_site(system, exploration_site_position(sde, system), Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, taken);
    // The far side's target IDs are drawn from its own system's set when it is generated;
    // a clash there is harmless (results are per system).
    let mut there = new_site(dest_system, exploration_site_position(sde, dest_system), Kind::Signature, STRENGTH_WORMHOLE, WORMHOLE_SCAN_EASE, &mut HashSet::new());
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
        nebula: a.destination_nebula.unwrap_or(0),
    };
    there.wormhole = Some(Wormhole {
        type_id: TYPE_K162,
        dest_system: system,
        dest_class: class,
        other: here.id,
        nebula: a.origin_nebula.unwrap_or(0),
        ..base.clone()
    });
    here.wormhole = Some(base);
    Ok(Some((here, there)))
}
/// Serialises loading and generation (so two pilots entering a system generate it once).
pub(super) fn load_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// Bring a system's sites into memory: load what is stored (closing wormholes past their
/// lifetime), generate the system's sites on its first visit ever, and put wormholes and
/// site containers that were in space back into space.
pub(super) async fn ensure_loaded(db: &Db, world: &Arc<World>, system: i64) -> Result<(), CallError> {
    if state().loaded.contains(&system) {
        return Ok(());
    }
    let _guard = load_lock().lock().await;
    if state().loaded.contains(&system) {
        return Ok(());
    }
    if NEXT_SITE_ID.load(Ordering::Relaxed) == 0 {
        let next = crate::dao::exploration::next_site_id(db).await.map_err(db_err)?;
        NEXT_SITE_ID.store(next, Ordering::Relaxed);
    }
    // Wormholes past their lifetime close at both ends.
    crate::dao::exploration::delete_expired(db, filetime_now()).await.map_err(db_err)?;
    let stored: Vec<Site> = crate::dao::exploration::sites(db, system).await.map_err(db_err)?.into_iter().map(site_from_row).collect();
    let generated = crate::dao::exploration::is_generated(db, system).await.map_err(db_err)?;
    let new = if generated {
        // Systems generated before mining sites existed get theirs now.
        if stored.iter().any(|s| s.mining_field().is_some()) {
            vec![]
        } else {
            let info = system_info(db, system).await?;
            let mut taken = stored.iter().map(|s| s.target.clone()).collect();
            mining_sites(db, world.sde(), system, &info, &mut taken).await?
        }
    } else {
        let taken = stored.iter().map(|s| s.target.clone()).collect();
        generate(db, world.sde(), system, taken).await?
    };
    for s in &new {
        save_site(db, s).await?;
    }
    if !generated {
        crate::dao::exploration::mark_generated(db, system, filetime_now()).await.map_err(db_err)?;
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
pub(super) async fn ensure_static_exit(db: &Db, world: &World, system: i64) -> Result<(), CallError> {
    let has_exit = system_sites(system).iter().any(|s| s.wormhole.as_ref().is_some_and(|w| (7..=9).contains(&w.dest_class)));
    if has_exit {
        return Ok(());
    }
    let info = system_info(db, system).await?;
    if !(1..=6).contains(&info.class) {
        return Ok(());
    }
    let mut taken: HashSet<String> = system_sites(system).into_iter().map(|s| s.target).collect();
    let Some((here, there)) = wormhole_pair(db, world.sde(), system, info.class, &[7, 8, 9], &mut taken).await? else { return Ok(()) };
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
pub(super) async fn generate(db: &Db, sde: &Sde, system: i64, mut taken: HashSet<String>) -> Result<Vec<Site>, CallError> {
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
        new.extend(combat_site(db, sde, system, &info, Kind::Anomaly, &mut taken).await?);
    }
    for _ in 0..combat_sigs {
        new.extend(combat_site(db, sde, system, &info, Kind::Signature, &mut taken).await?);
    }
    new.extend(mining_sites(db, sde, system, &info, &mut taken).await?);
    // A data and a relic site (two of each in null-sec).
    let hack_sites = if info.class == 9 { 2 } else { 1 };
    for _ in 0..hack_sites {
        for relic in [false, true] {
            new.extend(hack_site(db, sde, system, &info, relic, &mut taken).await?);
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
        if let Some((here, there)) = wormhole_pair(db, sde, system, info.class, targets, &mut taken).await? {
            new.push(here);
            new.push(there);
        }
    }
    info!("exploration: {system} (class {}, {:.2}) has {} new sites", info.class, info.security, new.len());
    Ok(new)
}

/// Ore anomalies everywhere, ice fields in some of known space, gas clouds to scan for.
pub(super) async fn mining_sites(db: &Db, sde: &Sde, system: i64, info: &SystemInfo, taken: &mut HashSet<String>) -> Result<Vec<Site>, CallError> {
    use crate::services::mining::Field;
    let jspace = (1..=6).contains(&info.class);
    let mut out = vec![];
    for _ in 0..if info.class == 9 { 2 } else { 1 } {
        out.extend(mining_site(db, sde, system, info, Field::Ore, taken).await?);
    }
    if !jspace && rand01() < 0.3 {
        out.extend(mining_site(db, sde, system, info, Field::Ice, taken).await?);
    }
    if jspace || info.class != 7 || rand01() < 0.5 {
        out.extend(mining_site(db, sde, system, info, Field::Gas, taken).await?);
    }
    Ok(out)
}

/// An ore or ice anomaly, or a gas signature, named after one of the build's sites.
pub(super) async fn mining_site(db: &Db, sde: &Sde, system: i64, info: &SystemInfo, field: crate::services::mining::Field, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
    use crate::services::mining::Field;
    let jspace = (1..=6).contains(&info.class);
    let (archetype, pattern) = match field {
        Field::Ore => (ARCHETYPE_ORE, if jspace { "%Perimeter%" } else { "%Deposit%" }),
        Field::Ice => (ARCHETYPE_ICE, "%Belt%"),
        Field::Gas => (ARCHETYPE_GAS, if jspace { "%Reservoir%" } else { "% Nebula" }),
    };
    let r = db
        .query_opt(
            "SELECT d.key::bigint AS id, (d.data->>'dungeonNameID')::bigint AS name_id
             FROM sde_native.dungeons d JOIN sde_native.localization_fsd_en_us l ON l.key = d.data->>'dungeonNameID'
             WHERE d.data->>'archetypeID' = $1 AND l.data->>'text' LIKE $2 AND l.data->>'text' NOT LIKE '%OLD%'
             ORDER BY random() LIMIT 1",
            &[&archetype.to_string(), &pattern],
        )
        .await
        .map_err(db_err)?;
    let Some(r) = r.as_ref().map(DungeonRow::from_row) else { return Ok(None) };
    let (kind, strength, ease) = match field {
        Field::Gas => (Kind::Signature, STRENGTH_LADAR, if jspace { 0.6 } else { 0.8 }),
        _ => (Kind::Anomaly, STRENGTH_GRAVIMETRIC, 1.0),
    };
    let mut site = new_site(system, exploration_site_position(sde, system), kind, strength, ease, taken);
    site.dungeon_id = Some(r.id);
    site.name_id = r.name_id;
    site.archetype = Some(archetype);
    site.difficulty = match info.class {
        7 => 1,
        8 => 2,
        _ => 3,
    };
    Ok(Some(site))
}

/// A data or relic site: the faction's (or the Sleepers') hackable ruins.
pub(super) async fn hack_site(db: &Db, sde: &Sde, system: i64, info: &SystemInfo, relic: bool, taken: &mut HashSet<String>) -> Result<Option<Site>, CallError> {
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
            "SELECT d.key::bigint AS id, (d.data->>'dungeonNameID')::bigint AS name_id, d.data->>'archetypeID' = $2 AS preferred
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
                "SELECT d.key::bigint AS id, (d.data->>'dungeonNameID')::bigint AS name_id, true AS preferred FROM sde_native.dungeons d
                 WHERE d.data->>'archetypeID' = $1 AND (d.data->>'factionID')::bigint = $2 AND d.data ? 'dungeonNameID'
                 ORDER BY random() LIMIT 1",
                &[&archetype, &faction],
            )
            .await
            .map_err(db_err)?,
    };
    let Some(r) = r.as_ref().map(HackDungeonRow::from_row) else { return Ok(None) };
    let strength = if relic { STRENGTH_MAGNETOMETRIC } else { STRENGTH_RADAR };
    let mut site = new_site(system, exploration_site_position(sde, system), Kind::Signature, strength, ease, taken);
    site.dungeon_id = Some(r.id);
    site.name_id = r.name_id;
    site.archetype = Some(if relic { 44 } else { 45 });
    site.faction = Some(faction);
    site.difficulty = tier.clamp(1, 4);
    Ok(Some(site))
}
