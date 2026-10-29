//! Server-side dogma for a character in its active ship: the effective attributes of
//! the character, ship, fitted modules and loaded charges with every skill, hull,
//! module and charge modifier applied (sde.dogma_effects.modifier_info), as the
//! client computes them for display (dogma/baseDogmaLocation).
//!
//! Active effects: passive (category 0) everywhere, online (4) on online modules, and
//! activation (1) on modules that are running (combat::active_modules). Propulsion
//! modules have no modifier data and use the game's formula instead (propulsion()).
//! Modifier functions: ItemModifier, LocationModifier, LocationGroupModifier,
//! LocationRequiredSkillModifier and OwnerRequiredSkillModifier, with the dogma
//! operations applied in order and stacking penalties on non-stackable attributes.

use super::{db_err, CallError};
use crate::consts::{ATTR_CAPACITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_RADIUS, ATTR_SIGNATURE_RADIUS, ATTR_VOLUME, CATEGORY_SHIP, EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, TYPE_CHARACTER};
use crate::db::Db;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

pub const ATTR_SKILL_LEVEL: i32 = 280;
/// requiredSkill1..6
const ATTR_REQUIRED_SKILLS: [i32; 6] = [182, 183, 184, 1285, 1289, 1290];
// Categories whose modifiers are exempt from stacking penalties.
const CATEGORY_CHARGE: i32 = 8;
const CATEGORY_SKILL: i32 = 16;
const CATEGORY_IMPLANT: i32 = 20;
const CATEGORY_SUBSYSTEM: i32 = 32;
/// SDE dogma effect category: passive.
const EFFECT_CATEGORY_PASSIVE: i32 = 0;
/// SDE dogma effect category: active.
const EFFECT_CATEGORY_ACTIVE: i32 = 1;
/// SDE dogma effect category: online.
const EFFECT_CATEGORY_ONLINE: i32 = 4;
const ATTR_SPEED_FACTOR: i32 = 20;
const ATTR_SPEED_BOOST_FACTOR: i32 = 567;
const ATTR_MASS_ADDITION: i32 = 796;
const ATTR_SIGNATURE_RADIUS_BONUS: i32 = 554;
/// skillEffect: skill point bookkeeping (operation 9), not a modifier we evaluate.
const EFFECT_SKILL: i32 = 132;

/// An item in the fit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Char,
    Ship,
    Module(i64),
    /// The charge loaded in a module.
    Charge(i64),
    Skill(i32),
}

#[derive(Clone)]
struct Modifier {
    category: i32,
    func: String,
    domain: String,
    op: i32,
    modified: i32,
    modifying: i32,
    group: Option<i32>,
    skill: Option<i32>,
}

/// Static data of a type.
struct TypeData {
    group: i32,
    category: i32,
    attrs: HashMap<i32, f64>,
    modifiers: Vec<Modifier>,
    effects: HashSet<i32>,
}

fn type_cache() -> &'static Mutex<HashMap<i32, Arc<TypeData>>> {
    static CACHE: OnceLock<Mutex<HashMap<i32, Arc<TypeData>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Attribute defaults and stackability.
fn attribute_meta() -> &'static Mutex<Option<Arc<HashMap<i32, (f64, bool)>>>> {
    static META: OnceLock<Mutex<Option<Arc<HashMap<i32, (f64, bool)>>>>> = OnceLock::new();
    META.get_or_init(Default::default)
}

async fn meta(db: &Db) -> Result<Arc<HashMap<i32, (f64, bool)>>, CallError> {
    if let Some(m) = attribute_meta().lock().unwrap().clone() {
        return Ok(m);
    }
    let rows = db
        .client
        .query("SELECT attribute_id, COALESCE(default_value, 0)::float8, COALESCE(stackable, true) FROM sde.dogma_attributes", &[])
        .await
        .map_err(db_err)?;
    let m = Arc::new(rows.iter().map(|r| (r.get::<_, i32>(0), (r.get::<_, f64>(1), r.get::<_, bool>(2)))).collect::<HashMap<_, _>>());
    *attribute_meta().lock().unwrap() = Some(m.clone());
    Ok(m)
}

/// Static data for `types`, from the cache or the SDE.
async fn types(db: &Db, types: &[i32]) -> Result<HashMap<i32, Arc<TypeData>>, CallError> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    {
        let cache = type_cache().lock().unwrap();
        for t in types {
            match cache.get(t) {
                Some(d) => {
                    out.insert(*t, d.clone());
                }
                None if !missing.contains(t) => missing.push(*t),
                None => {}
            }
        }
    }
    if missing.is_empty() {
        return Ok(out);
    }
    let base = db
        .client
        .query(
            "SELECT type_id, COALESCE(group_id, 0), COALESCE(category_id, 0), mass::float8, capacity::float8, volume::float8, radius::float8
             FROM sde.inv_types WHERE type_id = ANY($1)",
            &[&missing],
        )
        .await
        .map_err(db_err)?;
    let attrs = db
        .client
        .query("SELECT type_id, attribute_id, value FROM sde.type_attributes WHERE type_id = ANY($1)", &[&missing])
        .await
        .map_err(db_err)?;
    let mods = db
        .client
        .query(
            "SELECT te.type_id, COALESCE(e.effect_category, 0), m->>'func', COALESCE(m->>'domain', ''), COALESCE((m->>'operation')::int, -100),
                    COALESCE((m->>'modifiedAttributeID')::int, 0), COALESCE((m->>'modifyingAttributeID')::int, 0),
                    (m->>'groupID')::int, (m->>'skillTypeID')::int
             FROM sde.type_effects te JOIN sde.dogma_effects e USING (effect_id), jsonb_array_elements(e.modifier_info) m
             WHERE te.type_id = ANY($1) AND e.effect_id <> $2 AND jsonb_typeof(e.modifier_info) = 'array'",
            &[&missing, &EFFECT_SKILL],
        )
        .await
        .map_err(db_err)?;
    let effects = db.client.query("SELECT type_id, effect_id FROM sde.type_effects WHERE type_id = ANY($1)", &[&missing]).await.map_err(db_err)?;
    let mut built: HashMap<i32, TypeData> = base
        .iter()
        .map(|r| (r.get(0), TypeData { group: r.get(1), category: r.get(2), attrs: HashMap::new(), modifiers: vec![], effects: HashSet::new() }))
        .collect();
    for r in effects {
        if let Some(t) = built.get_mut(&r.get::<_, i32>(0)) {
            t.effects.insert(r.get(1));
        }
    }
    for r in attrs {
        if let Some(t) = built.get_mut(&r.get::<_, i32>(0)) {
            t.attrs.insert(r.get(1), r.get(2));
        }
    }
    // mass, capacity, volume and radius are type columns rather than attribute rows.
    for r in &base {
        if let Some(t) = built.get_mut(&r.get::<_, i32>(0)) {
            for (i, attr) in [ATTR_MASS, ATTR_CAPACITY, ATTR_VOLUME, ATTR_RADIUS].into_iter().enumerate() {
                if let Some(v) = r.get::<_, Option<f64>>(3 + i) {
                    t.attrs.entry(attr).or_insert(v);
                }
            }
        }
    }
    for r in mods {
        if let Some(t) = built.get_mut(&r.get::<_, i32>(0)) {
            t.modifiers.push(Modifier {
                category: r.get(1),
                func: r.get::<_, Option<String>>(2).unwrap_or_default(),
                domain: r.get(3),
                op: r.get(4),
                modified: r.get(5),
                modifying: r.get(6),
                group: r.get(7),
                skill: r.get(8),
            });
        }
    }
    let mut cache = type_cache().lock().unwrap();
    for (t, d) in built {
        let d = Arc::new(d);
        cache.insert(t, d.clone());
        out.insert(t, d);
    }
    Ok(out)
}

struct Node {
    type_id: i32,
    data: Arc<TypeData>,
    /// Located in the ship (modules, charges).
    in_ship: bool,
    online: bool,
    /// A module running its activation effect.
    active: bool,
    level: Option<i32>,
}

impl Node {
    fn required_skills(&self) -> impl Iterator<Item = i32> + '_ {
        ATTR_REQUIRED_SKILLS.iter().filter_map(|a| self.data.attrs.get(a)).map(|v| *v as i32).filter(|s| *s > 0)
    }
}

/// A character's active ship with everything that modifies it.
pub struct Fit {
    nodes: HashMap<Key, Node>,
    meta: Arc<HashMap<i32, (f64, bool)>>,
    memo: Mutex<HashMap<(Key, i32), f64>>,
}

impl Fit {
    /// Load the character, its skills, `ship`, its fitted modules and loaded charges.
    pub async fn load(db: &Db, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        let char_type: i32 = TYPE_CHARACTER;
        let ship_type: i32 = db
            .client
            .query_opt("SELECT type_id FROM game.items WHERE item_id = $1", &[&ship])
            .await
            .map_err(db_err)?
            .map(|r| r.get(0))
            .unwrap_or(0);
        let skills = db.client.query("SELECT type_id, level FROM game.skills WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?;
        let modules = db
            .client
            .query(
                "SELECT item_id, type_id, COALESCE(online, false), flag_id FROM game.items WHERE location_id = $1
                 AND (flag_id BETWEEN 11 AND 34 OR flag_id BETWEEN 92 AND 99 OR flag_id BETWEEN 125 AND 132)",
                &[&ship],
            )
            .await
            .map_err(db_err)?;
        let charges = db.client.query("SELECT flag_id, type_id FROM game.module_charges WHERE ship_id = $1", &[&ship]).await.map_err(db_err)?;
        let mut all: Vec<i32> = vec![char_type, ship_type];
        all.extend(skills.iter().map(|r| r.get::<_, i32>(0)));
        all.extend(modules.iter().map(|r| r.get::<_, i32>(1)));
        all.extend(charges.iter().map(|r| r.get::<_, i32>(1)));
        let data = types(db, &all).await?;
        let empty = || Arc::new(TypeData { group: 0, category: 0, attrs: HashMap::new(), modifiers: vec![], effects: HashSet::new() });
        let active = super::combat::active_modules();
        let get = |t: i32| data.get(&t).cloned().unwrap_or_else(empty);
        let mut nodes = HashMap::new();
        nodes.insert(Key::Char, Node { type_id: char_type, data: get(char_type), in_ship: false, online: true, active: false, level: None });
        nodes.insert(Key::Ship, Node { type_id: ship_type, data: get(ship_type), in_ship: false, online: true, active: false, level: None });
        for r in &skills {
            let t: i32 = r.get(0);
            nodes.insert(Key::Skill(t), Node { type_id: t, data: get(t), in_ship: false, online: true, active: false, level: Some(r.get(1)) });
        }
        let mut module_by_flag = HashMap::new();
        for r in &modules {
            let (id, t, online, flag): (i64, i32, bool, i32) = (r.get(0), r.get(1), r.get(2), r.get(3));
            module_by_flag.insert(flag, id);
            nodes.insert(Key::Module(id), Node { type_id: t, data: get(t), in_ship: true, online, active: online && active.contains(&id), level: None });
        }
        for r in &charges {
            let (flag, t): (i32, i32) = (r.get(0), r.get(1));
            if let Some(m) = module_by_flag.get(&flag) {
                nodes.insert(Key::Charge(*m), Node { type_id: t, data: get(t), in_ship: true, online: true, active: false, level: None });
            }
        }
        Ok(Fit { nodes, meta: meta(db).await?, memo: Mutex::new(HashMap::new()) })
    }

    fn base(&self, key: Key, attr: i32) -> f64 {
        let Some(node) = self.nodes.get(&key) else { return 0.0 };
        if attr == ATTR_SKILL_LEVEL {
            if let Some(level) = node.level {
                return level as f64;
            }
        }
        node.data.attrs.get(&attr).copied().unwrap_or_else(|| self.meta.get(&attr).map(|m| m.0).unwrap_or(0.0))
    }

    /// Does modifier `m` from `source` apply to `target`?
    fn applies(&self, source: Key, m: &Modifier, target: Key) -> bool {
        let Some(t) = self.nodes.get(&target) else { return false };
        let domain_item = match m.domain.as_str() {
            "itemID" => Some(source),
            "shipID" => Some(Key::Ship),
            "charID" => Some(Key::Char),
            "otherID" => match source {
                Key::Module(id) => Some(Key::Charge(id)),
                Key::Charge(id) => Some(Key::Module(id)),
                _ => None,
            },
            _ => None,
        };
        let required = |skill: Option<i32>| {
            let skill = match skill {
                Some(-1) | None => self.nodes.get(&source).map(|n| n.type_id),
                s => s,
            };
            skill.is_some_and(|s| t.required_skills().any(|r| r == s))
        };
        match m.func.as_str() {
            "ItemModifier" => domain_item == Some(target),
            // Location modifiers reach what is inside the ship (modules, charges); the
            // character "location" holds its skills and implants, which no bonus targets.
            "LocationModifier" => domain_item == Some(Key::Ship) && t.in_ship,
            "LocationGroupModifier" => domain_item == Some(Key::Ship) && t.in_ship && m.group == Some(t.data.group),
            "LocationRequiredSkillModifier" => domain_item == Some(Key::Ship) && t.in_ship && required(m.skill),
            "OwnerRequiredSkillModifier" => domain_item == Some(Key::Char) && t.in_ship && required(m.skill),
            _ => false,
        }
    }

    /// The effective value of `attr` on `key`.
    pub fn value(&self, key: Key, attr: i32) -> f64 {
        self.value_guarded(key, attr, &mut HashSet::new())
    }

    fn value_guarded(&self, key: Key, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> f64 {
        if let Some(v) = self.memo.lock().unwrap().get(&(key, attr)) {
            return *v;
        }
        let base = self.base(key, attr);
        if !visiting.insert((key, attr)) {
            return base;
        }
        // (operation, value, penalized)
        let mut mods: Vec<(i32, f64, bool)> = Vec::new();
        let stackable = self.meta.get(&attr).map(|m| m.1).unwrap_or(true);
        for (&source, node) in &self.nodes {
            for m in &node.data.modifiers {
                let running = match m.category {
                    EFFECT_CATEGORY_PASSIVE => true,
                    EFFECT_CATEGORY_ONLINE => node.online,
                    EFFECT_CATEGORY_ACTIVE => node.active,
                    _ => false,
                };
                if m.modified != attr || !running {
                    continue;
                }
                if !self.applies(source, m, key) {
                    continue;
                }
                let v = self.value_guarded(source, m.modifying, visiting);
                let exempt = matches!(node.data.category, CATEGORY_SHIP | CATEGORY_CHARGE | CATEGORY_SKILL | CATEGORY_IMPLANT | CATEGORY_SUBSYSTEM);
                mods.push((m.op, v, !stackable && !exempt));
            }
        }
        if key == Key::Ship {
            mods.extend(self.propulsion(attr, visiting));
        }
        visiting.remove(&(key, attr));
        let value = apply(base, &mods);
        self.memo.lock().unwrap().insert((key, attr), value);
        value
    }
}

impl Fit {
    /// Running afterburners and microwarpdrives: mass + massAddition, maxVelocity x
    /// (1 + speedFactor% x thrust / mass), and for microwarpdrives the signature bloom.
    fn propulsion(&self, attr: i32, visiting: &mut HashSet<(Key, i32)>) -> Vec<(i32, f64, bool)> {
        if !matches!(attr, ATTR_MASS | ATTR_MAX_VELOCITY | ATTR_SIGNATURE_RADIUS) {
            return vec![];
        }
        let mut mods = vec![];
        for (&key, node) in &self.nodes {
            let mwd = node.data.effects.contains(&EFFECT_MICROWARPDRIVE);
            if !node.active || !(mwd || node.data.effects.contains(&EFFECT_AFTERBURNER)) {
                continue;
            }
            match attr {
                ATTR_MASS => mods.push((2, self.value_guarded(key, ATTR_MASS_ADDITION, visiting), false)),
                ATTR_MAX_VELOCITY => {
                    let mass = self.value_guarded(Key::Ship, ATTR_MASS, visiting).max(1.0);
                    let factor = self.value_guarded(key, ATTR_SPEED_FACTOR, visiting) / 100.0;
                    let thrust = self.value_guarded(key, ATTR_SPEED_BOOST_FACTOR, visiting);
                    mods.push((4, 1.0 + factor * thrust / mass, false));
                }
                _ if mwd => mods.push((6, self.value_guarded(key, ATTR_SIGNATURE_RADIUS_BONUS, visiting), false)),
                _ => {}
            }
        }
        mods
    }
}

/// Apply dogma operations in order: PreAssign, PreMul, PreDiv, ModAdd, ModSub, PostMul,
/// PostDiv, PostPercent, PostAssign; penalized multipliers diminish (1 + (m-1)e^-(i/2.67)^2).
fn apply(base: f64, mods: &[(i32, f64, bool)]) -> f64 {
    let mut v = base;
    let of = |op: i32| mods.iter().filter(move |m| m.0 == op);
    if let Some(m) = of(-1).last() {
        v = m.1;
    }
    let multiply = |v: f64, factors: Vec<(f64, bool)>| -> f64 {
        let mut out = v;
        let (mut up, mut down): (Vec<f64>, Vec<f64>) = (vec![], vec![]);
        for (f, penalized) in factors {
            if !penalized {
                out *= f;
            } else if f >= 1.0 {
                up.push(f);
            } else {
                down.push(f);
            }
        }
        up.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        down.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for list in [up, down] {
            for (i, f) in list.into_iter().enumerate() {
                out *= 1.0 + (f - 1.0) * (-((i as f64) / 2.67).powi(2)).exp();
            }
        }
        out
    };
    v = multiply(v, of(0).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(1).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v += of(2).map(|m| m.1).sum::<f64>();
    v -= of(3).map(|m| m.1).sum::<f64>();
    v = multiply(v, of(4).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(5).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v = multiply(v, of(6).map(|m| (1.0 + m.1 / 100.0, m.2)).collect());
    if let Some(m) = of(7).last() {
        v = m.1;
    }
    v
}

#[cfg(test)]
mod tests {
    use super::apply;

    #[test]
    fn operations_and_stacking() {
        // 3 x 5% damage skill bonus (PostPercent, unpenalized) on 2.0
        assert!((apply(2.0, &[(6, 15.0, false)]) - 2.3).abs() < 1e-9);
        // Two 10% penalized damage mods: 1.1 * (1 + 0.1 * e^-(1/2.67)^2)
        let two = apply(1.0, &[(4, 1.1, true), (4, 1.1, true)]);
        assert!((two - 1.1 * (1.0 + 0.1 * (-(1.0f64 / 2.67).powi(2)).exp())).abs() < 1e-9);
        assert_eq!(apply(5.0, &[(2, 3.0, false), (7, 1.0, false)]), 1.0);
    }
}
