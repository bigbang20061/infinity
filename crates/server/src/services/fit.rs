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
use crate::consts::{ATTR_CAPACITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_RADIUS, ATTR_REQUIRED_SKILL_1, ATTR_SIGNATURE_RADIUS, ATTR_VOLUME, CATEGORY_CHARGE, CATEGORY_SHIP, EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, TYPE_CHARACTER};
use crate::dao::items::fit as fit_dao;
use crate::db::Db;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// SDE dgmAttributeTypes: skillLevel.
const ATTR_SKILL_LEVEL: i32 = 280;
/// SDE dgmAttributeTypes: requiredSkill1 through requiredSkill6.
const ATTR_REQUIRED_SKILLS: [i32; 6] = [ATTR_REQUIRED_SKILL_1, 183, 184, 1285, 1289, 1290];
/// SDE invCategories: Skill, exempt from stacking penalties.
const CATEGORY_SKILL: i32 = 16;
/// SDE invCategories: Implant, exempt from stacking penalties.
const CATEGORY_IMPLANT: i32 = 20;
/// SDE invCategories: Subsystem, exempt from stacking penalties.
const CATEGORY_SUBSYSTEM: i32 = 32;
/// SDE dogma effect category: passive.
const EFFECT_CATEGORY_PASSIVE: i32 = 0;
/// SDE dogma effect category: active.
const EFFECT_CATEGORY_ACTIVE: i32 = 1;
/// SDE dogma effect category: online.
const EFFECT_CATEGORY_ONLINE: i32 = 4;
/// SDE dgmAttributeTypes: speedFactor.
const ATTR_SPEED_FACTOR: i32 = 20;
/// SDE dgmAttributeTypes: speedBoostFactor.
const ATTR_SPEED_BOOST_FACTOR: i32 = 567;
/// SDE dgmAttributeTypes: massAddition.
const ATTR_MASS_ADDITION: i32 = 796;
/// SDE dgmAttributeTypes: signatureRadiusBonus.
const ATTR_SIGNATURE_RADIUS_BONUS: i32 = 554;
/// skillEffect: skill point bookkeeping (operation 9), not a modifier we evaluate.
const EFFECT_SKILL: i32 = 132;
/// SDE dogma modifier operation: PreAssign.
const OP_PRE_ASSIGN: i32 = -1;
/// SDE dogma modifier operation: PreMul.
const OP_PRE_MUL: i32 = 0;
/// SDE dogma modifier operation: PreDiv.
const OP_PRE_DIV: i32 = 1;
/// SDE dogma modifier operation: ModAdd.
const OP_MOD_ADD: i32 = 2;
/// SDE dogma modifier operation: ModSub.
const OP_MOD_SUB: i32 = 3;
/// SDE dogma modifier operation: PostMul.
const OP_POST_MUL: i32 = 4;
/// SDE dogma modifier operation: PostDiv.
const OP_POST_DIV: i32 = 5;
/// SDE dogma modifier operation: PostPercent.
const OP_POST_PERCENT: i32 = 6;
/// SDE dogma modifier operation: PostAssign.
const OP_POST_ASSIGN: i32 = 7;
/// Dogma stacking penalty curve divisor, used by the server fitting formula.
const STACKING_PENALTY_DIVISOR: f64 = 2.67;
/// Server dogma stacking penalty curve squares the normalized modifier index.
const STACKING_PENALTY_EXPONENT: i32 = 2;
/// Percentage points per whole multiplier.
const PERCENT_SCALE: f64 = 100.0;
/// Server sentinel for an SDE modifier with no operation, outside supported operations.
const MISSING_MODIFIER_OPERATION: i32 = -100;
/// SDE modifier skillTypeID sentinel selecting the source item's type.
const MODIFIER_SOURCE_ITEM_TYPE: i32 = -1;

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

struct AttributeMetaRow { attribute_id: i32, default_value: f64, stackable: bool }
impl AttributeMetaRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { attribute_id: r.get("attribute_id"), default_value: r.get("default_value"), stackable: r.get("stackable") }
    }
}
struct BaseRow { type_id: i32, group_id: i32, category_id: i32, mass: Option<f64>, capacity: Option<f64>, volume: Option<f64>, radius: Option<f64> }
impl BaseRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { type_id: r.get("type_id"), group_id: r.get("group_id"), category_id: r.get("category_id"), mass: r.get("mass"), capacity: r.get("capacity"), volume: r.get("volume"), radius: r.get("radius") }
    }
}
struct TypeAttributeRow { type_id: i32, attribute_id: i32, value: f64 }
impl TypeAttributeRow {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), attribute_id: r.get("attribute_id"), value: r.get("value") } }
}
struct TypeEffectRow { type_id: i32, effect_id: i32 }
impl TypeEffectRow {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), effect_id: r.get("effect_id") } }
}
struct ModifierRow { type_id: i32, category: i32, func: Option<String>, domain: String, op: i32, modified: i32, modifying: i32, group: Option<i32>, skill: Option<i32> }
impl ModifierRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { type_id: r.get("type_id"), category: r.get("category"), func: r.get("func"), domain: r.get("domain"), op: r.get("op"), modified: r.get("modified"), modifying: r.get("modifying"), group: r.get("group_id"), skill: r.get("skill_type_id") }
    }
}

async fn meta(db: &Db) -> Result<Arc<HashMap<i32, (f64, bool)>>, CallError> {
    if let Some(m) = attribute_meta().lock().unwrap().clone() {
        return Ok(m);
    }
    let rows = db
        .query("SELECT attribute_id, COALESCE(default_value, 0)::float8 AS default_value, COALESCE(stackable, true) AS stackable FROM sde.dogma_attributes", &[])
        .await
        .map_err(db_err)?;
    let m = Arc::new(rows.iter().map(AttributeMetaRow::from_row).map(|r| (r.attribute_id, (r.default_value, r.stackable))).collect::<HashMap<_, _>>());
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
        .query(
            "SELECT type_id, COALESCE(group_id, 0) AS group_id, COALESCE(category_id, 0) AS category_id, mass::float8, capacity::float8, volume::float8, radius::float8
             FROM sde.inv_types WHERE type_id = ANY($1)",
            &[&missing],
        )
        .await
        .map_err(db_err)?;
    let attrs = db
        .query("SELECT type_id, attribute_id, value FROM sde.type_attributes WHERE type_id = ANY($1)", &[&missing])
        .await
        .map_err(db_err)?;
    let mods = db
        .query(
            "SELECT te.type_id, COALESCE(e.effect_category, 0) AS category, m->>'func' AS func, COALESCE(m->>'domain', '') AS domain, COALESCE((m->>'operation')::int, $3) AS op,
                    COALESCE((m->>'modifiedAttributeID')::int, 0) AS modified, COALESCE((m->>'modifyingAttributeID')::int, 0) AS modifying,
                    (m->>'groupID')::int AS group_id, (m->>'skillTypeID')::int AS skill_type_id
             FROM sde.type_effects te JOIN sde.dogma_effects e USING (effect_id), jsonb_array_elements(e.modifier_info) m
             WHERE te.type_id = ANY($1) AND e.effect_id <> $2 AND jsonb_typeof(e.modifier_info) = 'array'",
            &[&missing, &EFFECT_SKILL, &MISSING_MODIFIER_OPERATION],
        )
        .await
        .map_err(db_err)?;
    let effects = db.query("SELECT type_id, effect_id FROM sde.type_effects WHERE type_id = ANY($1)", &[&missing]).await.map_err(db_err)?;
    let base: Vec<_> = base.iter().map(BaseRow::from_row).collect();
    let mut built: HashMap<i32, TypeData> = base
        .iter()
        .map(|r| (r.type_id, TypeData { group: r.group_id, category: r.category_id, attrs: HashMap::new(), modifiers: vec![], effects: HashSet::new() }))
        .collect();
    for r in effects.iter().map(TypeEffectRow::from_row) {
        if let Some(t) = built.get_mut(&r.type_id) {
            t.effects.insert(r.effect_id);
        }
    }
    for r in attrs.iter().map(TypeAttributeRow::from_row) {
        if let Some(t) = built.get_mut(&r.type_id) {
            t.attrs.insert(r.attribute_id, r.value);
        }
    }
    // mass, capacity, volume and radius are type columns rather than attribute rows.
    for r in &base {
        if let Some(t) = built.get_mut(&r.type_id) {
            for (attr, value) in [ATTR_MASS, ATTR_CAPACITY, ATTR_VOLUME, ATTR_RADIUS].into_iter().zip([r.mass, r.capacity, r.volume, r.radius]) {
                if let Some(v) = value {
                    t.attrs.entry(attr).or_insert(v);
                }
            }
        }
    }
    for r in mods.iter().map(ModifierRow::from_row) {
        if let Some(t) = built.get_mut(&r.type_id) {
            t.modifiers.push(Modifier {
                category: r.category,
                func: r.func.unwrap_or_default(),
                domain: r.domain,
                op: r.op,
                modified: r.modified,
                modifying: r.modifying,
                group: r.group,
                skill: r.skill,
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
        let ship_type = crate::dao::items::inventory::type_id(db, ship).await.map_err(db_err)?.unwrap_or(0);
        let skills = crate::dao::skills::levels::levels(db, char_id).await.map_err(db_err)?;
        let modules = fit_dao::modules(db, ship).await.map_err(db_err)?;
        let charges = fit_dao::charges(db, ship).await.map_err(db_err)?;
        let mut all: Vec<i32> = vec![char_type, ship_type];
        all.extend(skills.iter().map(|r| r.type_id));
        all.extend(modules.iter().map(|r| r.type_id));
        all.extend(charges.iter().map(|r| r.type_id));
        let data = types(db, &all).await?;
        let empty = || Arc::new(TypeData { group: 0, category: 0, attrs: HashMap::new(), modifiers: vec![], effects: HashSet::new() });
        let active = super::combat::active_modules();
        let get = |t: i32| data.get(&t).cloned().unwrap_or_else(empty);
        let mut nodes = HashMap::new();
        nodes.insert(Key::Char, Node { type_id: char_type, data: get(char_type), in_ship: false, online: true, active: false, level: None });
        nodes.insert(Key::Ship, Node { type_id: ship_type, data: get(ship_type), in_ship: false, online: true, active: false, level: None });
        for r in &skills {
            let t = r.type_id;
            nodes.insert(Key::Skill(t), Node { type_id: t, data: get(t), in_ship: false, online: true, active: false, level: Some(r.level) });
        }
        let mut module_by_flag = HashMap::new();
        for r in &modules {
            let (id, t, online, flag) = (r.item_id, r.type_id, r.online, r.flag_id);
            module_by_flag.insert(flag, id);
            nodes.insert(Key::Module(id), Node { type_id: t, data: get(t), in_ship: true, online, active: online && active.contains(&id), level: None });
        }
        for r in &charges {
            let (flag, t) = (r.flag_id, r.type_id);
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
                Some(MODIFIER_SOURCE_ITEM_TYPE) | None => self.nodes.get(&source).map(|n| n.type_id),
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
                ATTR_MASS => mods.push((OP_MOD_ADD, self.value_guarded(key, ATTR_MASS_ADDITION, visiting), false)),
                ATTR_MAX_VELOCITY => {
                    let mass = self.value_guarded(Key::Ship, ATTR_MASS, visiting).max(1.0);
                    let factor = self.value_guarded(key, ATTR_SPEED_FACTOR, visiting) / PERCENT_SCALE;
                    let thrust = self.value_guarded(key, ATTR_SPEED_BOOST_FACTOR, visiting);
                    mods.push((OP_POST_MUL, 1.0 + factor * thrust / mass, false));
                }
                _ if mwd => mods.push((OP_POST_PERCENT, self.value_guarded(key, ATTR_SIGNATURE_RADIUS_BONUS, visiting), false)),
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
    if let Some(m) = of(OP_PRE_ASSIGN).last() {
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
                out *= 1.0 + (f - 1.0) * (-((i as f64) / STACKING_PENALTY_DIVISOR).powi(STACKING_PENALTY_EXPONENT)).exp();
            }
        }
        out
    };
    v = multiply(v, of(OP_PRE_MUL).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(OP_PRE_DIV).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v += of(OP_MOD_ADD).map(|m| m.1).sum::<f64>();
    v -= of(OP_MOD_SUB).map(|m| m.1).sum::<f64>();
    v = multiply(v, of(OP_POST_MUL).map(|m| (m.1, m.2)).collect());
    v = multiply(v, of(OP_POST_DIV).filter(|m| m.1 != 0.0).map(|m| (1.0 / m.1, m.2)).collect());
    v = multiply(v, of(OP_POST_PERCENT).map(|m| (1.0 + m.1 / PERCENT_SCALE, m.2)).collect());
    if let Some(m) = of(OP_POST_ASSIGN).last() {
        v = m.1;
    }
    v
}

#[cfg(test)] mod tests;
