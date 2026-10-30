//! Exploration sites mechanics.
//! Shared exploration state and data sources are documented in the parent module.

use super::*;

struct FactionCorp { corporation_id: Option<i64> }
impl FactionCorp { fn from_row(r: &tokio_postgres::Row) -> Self { Self { corporation_id: r.get("corporation_id") } } }
struct RatType { type_id: i32, radius: f64, mass: f64, max_vel: Option<f64>, agility: Option<f64> }
impl RatType { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), radius: r.get("radius"), mass: r.get("mass"), max_vel: r.get("max_vel"), agility: r.get("agility") } } }
struct ContainerType { type_id: i32, radius: f64 }
impl ContainerType { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), radius: r.get("radius") } } }

// ---------------------------------------------------------------- site content

/// NPC hull classes per site difficulty.
pub(super) fn site_classes(difficulty: i64) -> &'static [&'static str] {
    match difficulty {
        ..=1 => &["Frigate", "Frigate", "Frigate", "Destroyer"],
        2 => &["Frigate", "Frigate", "Destroyer", "Cruiser", "Cruiser"],
        3 => &["Destroyer", "Cruiser", "Cruiser", "BattleCruiser", "BattleCruiser"],
        4 => &["Cruiser", "BattleCruiser", "BattleCruiser", "Battleship", "Battleship"],
        _ => &["BattleCruiser", "Battleship", "Battleship", "Battleship", "Battleship", "Cruiser"],
    }
}

/// NPC group name prefixes of a faction's site rats.
pub(super) fn rat_prefix(faction: i64) -> &'static str {
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
pub(super) fn sleeper_groups(difficulty: i64) -> [&'static str; 3] {
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
pub(super) async fn spawn_site_npcs(c: &mut Client, site: &Site) -> Result<(), CallError> {
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
pub(super) fn track_site_clearance(db: Db, world: Arc<World>, site_id: i64, system: i64, members: Vec<i64>) {
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
pub(super) async fn spawn_rats(c: &mut Client, system: i64, center: V3, faction: i64, groups: &[String], spread: (f64, f64)) -> Result<Vec<i64>, CallError> {
    let corp: i64 = c
        .db
        .query_opt("SELECT corporation_id::bigint AS corporation_id FROM sde.chr_factions WHERE faction_id::bigint = $1", &[&faction])
        .await
        .map_err(db_err)?
        .and_then(|r| FactionCorp::from_row(&r).corporation_id)
        .unwrap_or(1000127);
    let ms = now_ms();
    let mut spawned = vec![];
    for group in groups {
        let Some(r) = c
            .db
            .query_opt(
                "SELECT t.type_id, COALESCE(t.radius, 50)::float8 AS radius, COALESCE(t.mass, 1000000)::float8 AS mass,
                        (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 37) AS max_vel,
                        (SELECT value FROM sde.type_attributes WHERE type_id = t.type_id AND attribute_id = 70) AS agility
                 FROM sde.inv_types t JOIN sde.inv_groups g ON g.group_id = t.group_id
                 WHERE g.category_id = 11 AND g.name = $1 ORDER BY random() LIMIT 1",
                &[group],
            )
            .await
            .map_err(db_err)?
        else {
            continue;
        };
        let r = RatType::from_row(&r);
        let d = spread.0 + rand01() * (spread.1 - spread.0);
        let v = rand_dir();
        let ball = Ball {
            id: crate::services::missions::NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
            type_id: r.type_id,
            owner_id: corp,
            corp_id: corp,
            alliance_id: -1,
            char_id: None,
            signature_radius: None,
            mode: MODE_STOP,
            radius: r.radius as f32,
            mass: r.mass,
            max_vel: r.max_vel.unwrap_or(300.0) as f32,
            agility: r.agility.unwrap_or(3.0) as f32,
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
        crate::services::combat::spawn_npc(c.db.clone(), c.world.clone(), system, id, type_id, Some(crate::space::HOSTILE_TO_ALL)).await?;
        spawned.push(id);
    }
    Ok(spawned)
}

// ---------------------------------------------------------------- belt rats

/// Belt rat hull classes by security: a few frigates in high-sec, up to battleships in null.
pub(super) fn belt_classes(security: f64) -> &'static [&'static str] {
    if security >= 0.45 {
        &["Frigate", "Frigate", "Destroyer"]
    } else if security > 0.0 {
        &["Frigate", "Destroyer", "Cruiser", "Cruiser"]
    } else {
        &["Cruiser", "BattleCruiser", "BattleCruiser", "Battleship"]
    }
}
/// A finished site leaves (with any containers) and one of the same kind appears
/// elsewhere in the system.
pub(super) async fn replace_site(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
    let Some(old) = state().sites.get(&site_id).cloned() else { return Ok(()) };
    let system = old.system;
    let info = system_info(db, system).await?;
    let mut taken: HashSet<String> = system_sites(system).into_iter().filter(|s| s.id != site_id).map(|s| s.target).collect();
    let new = if let Some(field) = old.mining_field() {
        mining_site(db, world.sde(), system, &info, field, &mut taken).await?
    } else if old.is_hack_site() {
        hack_site(db, world.sde(), system, &info, old.strength_attr == STRENGTH_MAGNETOMETRIC, &mut taken).await?
    } else {
        combat_site(db, world.sde(), system, &info, old.kind, &mut taken).await?
    };
    enum Replacement { Apply(Site, Option<Site>) }
    db.transaction(
        move |tx, outbox: &mut Outbox<Replacement>| {
            let (old, new) = (old.clone(), new.clone());
            Box::pin(async move {
                delete_site(tx, old.id).await?;
                for id in old.containers.iter().flatten() {
                    crate::dao::exploration::delete_container(tx, *id).await.map_err(db_err)?;
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
pub(super) fn wormhole_age(w: &Wormhole) -> i64 {
    let left = 1.0 - (filetime_now() - w.born) as f64 / (minutes_to_seconds(w.life_minutes) * FILETIME_SECOND as f64);
    match left {
        l if l > 0.5 => 0,
        l if l > 0.25 => 1,
        l if l > 0.05 => 2,
        _ => 3,
    }
}

/// slimItem.wormholeSize: 1.0 fresh, 0.7 reduced, 0.4 critical.
pub(super) fn wormhole_size(w: &Wormhole) -> f64 {
    match 1.0 - w.mass_used / w.mass_max.max(1.0) {
        l if l > 0.5 => 1.0,
        l if l > 0.1 => 0.7,
        _ => 0.4,
    }
}

/// appConst.WH_SLIM_MAX_SHIP_MASS_*
pub(super) fn jump_mass_class(jump_mass: f64) -> i64 {
    match jump_mass {
        m if m <= 5_000_000.0 => 1,
        m if m <= 62_000_000.0 => 2,
        m if m <= 1_000_000_000.0 => 3,
        _ => 4,
    }
}

pub(super) fn wormhole_ball(s: &Site, w: &Wormhole) -> Ball {
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
pub(super) async fn ensure_wormhole_ball(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
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
pub(super) fn connection(site: i64) -> Option<i64> {
    state().sites.get(&site).and_then(|s| s.wormhole.as_ref().map(|w| site.min(w.other)))
}

/// wormholeMgr.GetWormholePolarization(wormholeID) -> (endTime, duration) while the pilot
/// can't jump this connection again (spacecomponents jumpPolarization), else None.
pub(super) fn polarization(char_id: i64, site: i64) -> Option<(i64, i64)> {
    let connection = connection(site)?;
    let until = *state().polarized.get(&(char_id, connection))?;
    (until > filetime_now()).then_some((until, POLARIZATION_SECS * FILETIME_SECOND))
}
/// wormholeMgr.WormholeJump(wormholeID): through to the other side, next to its K162.
pub(super) async fn wormhole_jump(c: &mut Client, wormhole_id: i64) -> CallResult {
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
    let jump = crate::services::ship::Jump { through: wormhole_id, guid: "effects.JumpOutWormhole", other_type: Some(w.dest_class), dest_system: w.dest_system, pos };
    crate::services::ship::jump_out_and_arrive(c, system, ship, char_id, jump).await?;
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
pub(super) async fn collapse(db: &Db, world: &World, site_id: i64) -> Result<(), CallError> {
    let (ids, sites) = {
        let st = state();
        let other = st.sites.get(&site_id).and_then(|s| s.wormhole.as_ref().map(|w| w.other));
        let ids: Vec<_> = std::iter::once(site_id).chain(other).collect();
        let (first, second) = match other {
            Some(other) if other != site_id => (site_id.min(other), Some(site_id.max(other))),
            _ => (site_id, None),
        };
        let sites: Vec<_> = std::iter::once(first).chain(second).filter_map(|id| st.sites.get(&id).cloned()).collect();
        (ids, sites)
    };
    enum Collapse { Remove(Vec<Site>) }
    db.transaction(
        move |tx, outbox: &mut Outbox<Collapse>| {
            let (mut ids, sites) = (ids.clone(), sites.clone());
            Box::pin(async move {
                crate::dao::exploration::delete_sites(tx, &mut ids).await.map_err(db_err)?;
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
pub(super) async fn expire_wormholes(db: &Db, world: &World) -> Result<(), CallError> {
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
pub(super) fn container_prefix(faction: i64) -> &'static str {
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
pub(super) fn loot_groups(relic: bool, sleeper: bool) -> Vec<i32> {
    match (relic, sleeper) {
        (false, false) => vec![333, 728, 729, 730, 731, 733, 734, 735, 979, 1304],
        (false, true) => vec![880, 732, 333],
        (true, false) => vec![754],
        (true, true) => vec![966, 964],
    }
}

pub(super) fn container_ball(id: i64, type_id: i32, pos: V3, radius: f32, security_state: i64) -> Ball {
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
pub(super) async fn spawn_containers(c: &mut Client, site: &Site) -> Result<(), CallError> {
    let relic = site.strength_attr == STRENGTH_MAGNETOMETRIC;
    let faction = site.faction.unwrap_or(FACTION_GURISTAS);
    let prefix = container_prefix(faction);
    let sleeper = prefix == "Sleeper";
    let skill = if relic { SKILL_ARCHAEOLOGY } else { SKILL_HACKING };
    let types = c
        .db
        .query(
            "SELECT t.type_id, COALESCE(t.radius, 300)::float8 AS radius FROM sde.inv_types t
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
    let types: Vec<(i32, f64)> = types.iter().map(|r| { let r = ContainerType::from_row(r); (r.type_id, r.radius) }).collect();
    let count = 3 + (rand01() * 3.0) as usize;
    let value = CONTAINER_LOOT_VALUE[(site.difficulty.clamp(1, 4) - 1) as usize];
    let groups = loot_groups(relic, sleeper);
    let mut ids = vec![];
    for _ in 0..count {
        let Some((type_id, radius)) = pick(&types) else { break };
        let d = 2_000.0 + rand01() * 18_000.0;
        let v = rand_dir();
        let pos = [site.pos[0] + v[0] * d, site.pos[1] + v[1] * d * 0.3, site.pos[2] + v[2] * d];
        let id = crate::dao::exploration::insert_container(&c.db, type_id, site.system, pos).await.map_err(db_err)?;
        let stacks = 2 + (rand01() * 2.0) as i64;
        let loot = crate::dao::exploration::loot(&c.db, &groups, value, stacks).await.map_err(db_err)?;
        for r in &loot {
            let (loot_type, price) = (r.type_id, r.average);
            let quantity = ((value / stacks as f64 / price.max(1.0)) * (0.5 + rand01())).round().clamp(1.0, 500.0) as i32;
            crate::dao::exploration::insert_loot(&c.db, loot_type, id, quantity).await.map_err(db_err)?;
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
pub(super) async fn restore_containers(db: &Db, world: &World, site: &Site, ids: &[i64]) -> Result<(), CallError> {
    let rows = crate::dao::exploration::containers(db, ids).await.map_err(db_err)?;
    for r in rows {
        let id = r.item_id;
        let security = if site.hacked.contains(&id) { 2 } else { 0 };
        let ball = container_ball(id, r.type_id, r.pos, r.radius as f32, security);
        world.add_ship(site.system, ball).await.map_err(CallError::Internal)?;
    }
    Ok(())
}
