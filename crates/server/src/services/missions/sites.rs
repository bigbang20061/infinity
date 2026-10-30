//! Mission site and NPC mechanics, mirroring agent encounters in
//! `eve/client/script/ui/shared/agencyNew/` and using startup SDE types and factions.
//! Sites are transient; authored dungeon layouts and persistent site state are unsupported.
use super::*;
use crate::services::sql_pattern::matches_pattern;
use crate::consts::{ATTR_AGILITY, ATTR_MAX_VELOCITY, CATEGORY_ENTITY};

struct Npc<'a> {
    type_id: i32,
    name: &'a str,
    radius: f64,
    mass: f64,
    velocity: Option<f64>,
    agility: Option<f64>,
    generic: bool,
}


fn candidate_group(name: &str, faction: &str) -> Option<bool> {
    let generic = matches_pattern(name, "mission generic%");
    let first_word = faction.split(' ').next().unwrap_or("");
    // SQL lower maps per character, so ASCII prefix/suffix plus the SQL-folded
    // faction (or its first word) equals lower of the complete pattern.
    (generic
        || matches_pattern(name, &format!("mission {faction} %"))
        || matches_pattern(name, &format!("deadspace {first_word}%")))
        .then_some(generic)
}

fn preferred<'a, 'b>(rows: &'a [Npc<'b>], class: &str) -> Vec<&'a Npc<'b>> {
    let fits: Vec<_> = rows.iter().filter(|r| group_has_class(r.name, class)).collect();
    if fits.iter().any(|r| !r.generic) {
        fits.into_iter().filter(|r| !r.generic).collect()
    } else {
        fits
    }
}
// ---------------------------------------------------------------- sites and NPCs

struct MissionSite {
    system: i64,
    npcs: Vec<i64>,
    group: u64,
}

/// (character, agent) -> system, spawned NPCs, and actor group for the site.
fn sites() -> &'static Mutex<HashMap<(i64, i64), MissionSite>> {
    static SITES: OnceLock<Mutex<HashMap<(i64, i64), MissionSite>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// NPC hull classes per agent level.
pub(super) fn spawn_classes(level: i64) -> Vec<&'static str> {
    let (classes, n): (&[&str], usize) = match level {
        1 => (&["Frigate", "Frigate", "Destroyer"], 4),
        2 => (&["Frigate", "Destroyer", "Cruiser"], 5),
        3 => (&["Cruiser", "Cruiser", "Battlecruiser"], 6),
        4 => (&["Battlecruiser", "Battleship", "Battleship"], 7),
        _ => (&["Battleship"], 8),
    };
    (0..n).map(|i| classes[i % classes.len()]).collect()
}

/// Does an NPC group name (`Mission Caldari State Cruiser`, `Deadspace Guristas Battleship`,
/// `Mission Generic Cruisers`) carry ships of `class`?
pub(super) fn group_has_class(group: &str, class: &str) -> bool {
    let g = group.to_ascii_lowercase().replace("battle cruiser", "battlecruiser");
    let class = class.to_ascii_lowercase();
    match class.as_str() {
        "cruiser" => g.contains("cruiser") && !g.contains("battlecruiser"),
        c => g.contains(c),
    }
}

/// Spawn the mission's NPCs around its site, unless already there.
pub(super) async fn ensure_site(c: &mut Client, m: &Mission) -> Result<(), CallError> {
    let me = c.session.int("charid").unwrap_or(0);
    if sites().lock().unwrap().contains_key(&(me, m.agent_id)) {
        return Ok(());
    }
    let faction = c.sde.factions.get(m.faction_id);
    let faction_name = faction.and_then(|f| f.lower_name.as_deref()).unwrap_or("~");
    let rows: Vec<_> = c.sde.types.iter().filter_map(|t| {
        let group = c.sde.types.group(t.group_id?)?;
        if group.category_id != Some(CATEGORY_ENTITY) { return None; }
        let name = group.name.as_deref()?;
        let generic = candidate_group(group.lower_name.as_deref()?, faction_name)?;
        Some(Npc {
            type_id: t.type_id, name, radius: t.radius.unwrap_or(50.0),
            mass: t.mass.unwrap_or(1_000_000.0),
            velocity: t.attribute(ATTR_MAX_VELOCITY), agility: t.attribute(ATTR_AGILITY), generic,
        })
    }).collect();
    // NPCs belong to the faction's NPC corporation: the client looks owners and corpIDs up
    // as corporations (tickers, standings), and a faction ID there breaks target locking.
    let corp = faction.and_then(|f| f.corporation_id).unwrap_or(GENERIC_NPC_CORP);
    let level = agent(m.agent_id).map(|a| a.level).unwrap_or(1);
    let tick = now_ms();
    let mut spawned = Vec::new();
    for class in spawn_classes(level) {
        let pool = preferred(&rows, class);
        if pool.is_empty() {
            continue;
        }
        let r = pool[(rand01() * pool.len() as f64) as usize % pool.len()];
        let d = rand_dir();
        let dist = 12_000.0 + rand01() * 20_000.0;
        let ball = Ball {
            id: NEXT_NPC_ID.fetch_add(1, Ordering::Relaxed),
            type_id: r.type_id,
            owner_id: corp,
            corp_id: corp,
            alliance_id: -1,
            char_id: None,
            mode: MODE_STOP,
            radius: r.radius as f32,
            signature_radius: None,
            mass: r.mass,
            max_vel: r.velocity.unwrap_or(300.0) as f32,
            agility: r.agility.unwrap_or(3.0) as f32,
            speed_fraction: 0.0,
            pos: [m.pos[0] + d[0] * dist, m.pos[1] + d[1] * dist * 0.3, m.pos[2] + d[2] * dist],
            vel: [0.0; 3],
            goto: [0.0; 3],
            follow_id: 0,
            follow_range: 0.0,
            free: true,
            global: false,
            massive: true,
            warp: None,
            warp_factor: 0,
            last_ms: tick,
            cloak: 0,
            cloak_until: 0,
            health: [1.0; 3],
            launcher: 0,
            slim: vec![("corpID", Value::Int(corp))],
        };
        spawned.push(ball.id);
        let (id, type_id) = (ball.id, ball.type_id);
        c.world.add_ship(m.system_id, ball).await.map_err(CallError::Internal)?;
        super::combat::spawn_npc(c.db.clone(), c.world.clone(), m.system_id, id, type_id, Some(me)).await?;
    }
    let group_members = spawned.clone();
    let group = c.world.with(m.system_id, move |s| s.spawn_group(group_members, |_| {})).await
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("system actor stopped")))?;
    info!(user = c.login.user_id, "agent {}: spawned {} NPCs for mission {} in {}", m.agent_id, spawned.len(), m.mission_id, m.system_id);
    sites().lock().unwrap().insert((me, m.agent_id), MissionSite { system: m.system_id, npcs: spawned, group });
    Ok(())
}

pub(super) fn despawn(c: &Client, char_id: i64, agent_id: i64) {
    if let Some(site) = sites().lock().unwrap().remove(&(char_id, agent_id)) {
        c.world.post(site.system, move |s| {
            for id in site.npcs {
                s.remove_ball(id);
            }
        });
    }
}

/// Is the accepted mission's spawned NPC group cleared?
pub(super) async fn objective_done(c: &Client, m: &Mission) -> bool {
    if m.state != STATE_ACCEPTED { return false; }
    let me = c.session.int("charid").unwrap_or(0);
    let Some((system, group)) = sites().lock().unwrap().get(&(me, m.agent_id)).map(|site| (site.system, site.group)) else { return false };
    c.world.with(system, move |s| s.group_remaining(group) == 0).await.unwrap_or(false)
}


#[cfg(test)]
mod tests;
