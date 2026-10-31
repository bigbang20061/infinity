//! Server-side effective attributes for the active character, ship, fitted modules
//! and charges. Static attributes, effects and modifiers come from the shared SDE;
//! fitted items, per-item base overrides, charges and skill levels come from the runtime game database.
//! Client modifier classes and domains: `dogma/effects/modifiers.py:8-25,48-140`;
//! operation precedence and stacking categories:
//! `dogma/const.py:7-51`.
//!
//! This implements ItemModifier, LocationModifier, LocationGroupModifier,
//! LocationRequiredSkillModifier and OwnerRequiredSkillModifier for its fitted
//! character/ship domains, operations PreAssign through PostAssign, and stacking
//! penalties. Other modifier functions and operations, including PostPercentUnNerfed
//! and the special skill relationship, are not evaluated; this is not full client
//! dogma parity. Passive, online and running activation effects are considered;
//! audited migrated Python skill effects without SDE modifier rows are included.
//! Skill traversal excludes implants and boosters; propulsion uses a server formula.
//! ponytail: the audited SDE has no skill effect whose fittingUsageChance attribute
//! exists on its skill type; if that changes, load that field and filter such effects.

use super::{db_err, CallError};
use crate::consts::{ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_REQUIRED_SKILL_1, ATTR_SIGNATURE_RADIUS, CATEGORY_CHARGE, CATEGORY_SHIP, EFFECT_AFTERBURNER, EFFECT_MICROWARPDRIVE, EFFECT_SKILL, TYPE_CHARACTER};
use crate::consts::{ATTR_DAMAGE_MULTIPLIER, ATTR_RATE_OF_FIRE, ATTR_EM_DAMAGE, ATTR_THERMAL_DAMAGE, ATTR_KINETIC_DAMAGE, ATTR_EXPLOSIVE_DAMAGE};
use crate::dao::items::fit as fit_dao;
use crate::db::Db;
use crate::sde::{dogma::{Attribute, Modifier}, types::TypeData, Sde};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// SDE dgmAttributeTypes: skillLevel.
pub(super) const ATTR_SKILL_LEVEL: i32 = 280;
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
/// SDE dogma modifier operation: PreAssign.
pub(crate) const OP_PRE_ASSIGN: i32 = -1;
/// SDE dogma modifier operation: PreMul.
pub(crate) const OP_PRE_MUL: i32 = 0;
/// SDE dogma modifier operation: PreDiv.
pub(crate) const OP_PRE_DIV: i32 = 1;
/// SDE dogma modifier operation: ModAdd.
pub(crate) const OP_MOD_ADD: i32 = 2;
/// SDE dogma modifier operation: ModSub.
pub(crate) const OP_MOD_SUB: i32 = 3;
/// SDE dogma modifier operation: PostMul.
pub(crate) const OP_POST_MUL: i32 = 4;
/// SDE dogma modifier operation: PostDiv.
pub(crate) const OP_POST_DIV: i32 = 5;
/// SDE dogma modifier operation: PostPercent.
pub(crate) const OP_POST_PERCENT: i32 = 6;
/// SDE dogma modifier operation: PostAssign.
pub(crate) const OP_POST_ASSIGN: i32 = 7;
/// Dogma stacking penalty curve divisor, used by the server fitting formula.
const STACKING_PENALTY_DIVISOR: f64 = 2.67;
/// Server dogma stacking penalty curve squares the normalized modifier index.
const STACKING_PENALTY_EXPONENT: i32 = 2;
/// Percentage points per whole multiplier.
pub(crate) const PERCENT_SCALE: f64 = 100.0;
/// SDE modifier skillTypeID sentinel selecting the source item's type.
const MODIFIER_SOURCE_ITEM_TYPE: i32 = -1;
/// Client dogma/const.py attribute damageMultiplierBonus.
const ATTR_DAMAGE_MULTIPLIER_BONUS: i32 = 292;
/// Client dogma/const.py attribute cloakingTargetingDelay.
const ATTR_CLOAKING_TARGETING_DELAY: i32 = 560;
/// Client dogma/const.py attribute cloakingTargetingDelayBonus.
const ATTR_CLOAKING_TARGETING_DELAY_BONUS: i32 = 619;
/// Client dogma/const.py attribute rofBonus.
const ATTR_ROF_BONUS: i32 = 293;

/// Python effects without SDE modifier rows, from dogma/pythonEffects/migratedeffects.py.
fn migrated_skill_modifiers() -> &'static HashMap<&'static str, Modifier> {
    static MODIFIERS: OnceLock<HashMap<&'static str, Modifier>> = OnceLock::new();
    MODIFIERS.get_or_init(|| [
        ("missileEMDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EM_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileExplosiveDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EXPLOSIVE_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileKineticDmgBonus2", "OwnerRequiredSkillModifier", "charID", ATTR_KINETIC_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileThermalDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_THERMAL_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("droneDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_DAMAGE_MULTIPLIER, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("cloakingTargetingDelayBonusPostPercentCloakingTargetingDelayBonusForShipModulesRequiringCloaking", "LocationRequiredSkillModifier", "shipID", ATTR_CLOAKING_TARGETING_DELAY, ATTR_CLOAKING_TARGETING_DELAY_BONUS),
        ("selfRof", "LocationRequiredSkillModifier", "shipID", ATTR_RATE_OF_FIRE, ATTR_ROF_BONUS),
    ].into_iter().map(|(name, func, domain, modified, modifying)| {
        (name, Modifier { func: func.into(), domain: domain.into(), op: OP_POST_PERCENT, modified, modifying, group: None, skill: Some(MODIFIER_SOURCE_ITEM_TYPE) })
    }).collect())
}


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

struct Node {
    type_id: i32,
    // Positions remain stable in the immutable types owned by the retained Arc<Sde>.
    data_position: Option<usize>,
    /// Located in the ship (modules, charges).
    in_ship: bool,
    online: bool,
    /// A module running its activation effect.
    active: bool,
    level: Option<i32>,
    overrides: Vec<(i32, f64)>,
}

pub(crate) fn base_attribute(data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
    data.and_then(|t| t.fit_attribute(attr))
        .unwrap_or_else(|| meta.and_then(|m| m.default_value).unwrap_or(0.0))
}

impl Node {
    fn base(&self, data: Option<&TypeData>, meta: Option<&Attribute>, attr: i32) -> f64 {
        if attr == ATTR_SKILL_LEVEL {
            if let Some(level) = self.level { return level as f64; }
        }
        if let Ok(index) = self.overrides.binary_search_by_key(&attr, |entry| entry.0) {
            return self.overrides[index].1;
        }
        base_attribute(data, meta, attr)
    }
}


// SDE positions remain stable in the immutable Arc<Sde>; fallbacks are static.
enum PassiveModifier {
    Sde { effect_id: i32, position: usize },
    Fallback(&'static Modifier),
}

/// A character's active ship with everything that modifies it.
pub struct Fit {
    nodes: HashMap<Key, Node>,
    passive_modifiers: Vec<(Key, PassiveModifier)>,
    passive_by_attribute: HashMap<i32, Vec<usize>>,
    sde: Arc<Sde>,
    memo: Mutex<HashMap<(Key, i32), f64>>,
}

impl Fit {
    /// Load the character, its skills, `ship`, its fitted modules and loaded charges.
    pub async fn load(db: &Db, sde: Arc<Sde>, char_id: i64, ship: i64) -> Result<Fit, CallError> {
        let char_type: i32 = TYPE_CHARACTER;
        let ship_type = crate::dao::items::inventory::type_id(db, ship).await.map_err(db_err)?.unwrap_or(0);
        let skills = crate::dao::skills::levels::levels(db, char_id).await.map_err(db_err)?;
        let modules = fit_dao::modules(db, ship).await.map_err(db_err)?;
        let charges = fit_dao::charges(db, ship).await.map_err(db_err)?;
        let ids: Vec<i64> = std::iter::once(ship).chain(modules.iter().map(|m| m.item_id)).collect();
        let mut overrides: HashMap<i64, Vec<(i32, f64)>> = HashMap::new();
        for row in crate::dao::items::dogma::overrides_for(db, &ids).await.map_err(db_err)? {
            overrides.entry(row.item_id).or_default().push((row.attribute_id, row.value));
        }
        let active = super::combat::active_modules();
        let mut nodes = HashMap::new();
        nodes.insert(Key::Char, Node { type_id: char_type, data_position: sde.types.position(char_type), in_ship: false, online: true, active: false, level: None, overrides: Vec::new() });
        nodes.insert(Key::Ship, Node { type_id: ship_type, data_position: sde.types.position(ship_type), in_ship: false, online: true, active: false, level: None, overrides: overrides.remove(&ship).unwrap_or_default() });
        for r in &skills {
            let t = r.type_id;
            nodes.insert(Key::Skill(t), Node { type_id: t, data_position: sde.types.position(t), in_ship: false, online: true, active: false, level: Some(r.level), overrides: Vec::new() });
        }
        let mut module_by_flag = HashMap::new();
        for r in &modules {
            let (id, t, online, flag) = (r.item_id, r.type_id, r.online, r.flag_id);
            module_by_flag.insert(flag, id);
            nodes.insert(Key::Module(id), Node { type_id: t, data_position: sde.types.position(t), in_ship: true, online, active: online && active.contains(&id), level: None, overrides: overrides.remove(&id).unwrap_or_default() });
        }
        for r in &charges {
            let (flag, t) = (r.flag_id, r.type_id);
            if let Some(m) = module_by_flag.get(&flag) {
                nodes.insert(Key::Charge(*m), Node { type_id: t, data_position: sde.types.position(t), in_ship: true, online: true, active: false, level: None, overrides: Vec::new() });
            }
        }
        let mut skill_effects = Vec::new();
        for r in &skills {
            if r.level <= 0 { continue; }
            if let Some(data) = sde.types.get(r.type_id) {
                for entry in data.effects() {
                    if entry.effect_id == EFFECT_SKILL { continue; }
                    if sde.dogma.effect(entry.effect_id).is_some_and(|effect| effect.effect_category.unwrap_or(0) == EFFECT_CATEGORY_PASSIVE) {
                        skill_effects.push((r.type_id, entry.effect_id));
                    }
                }
            }
        }
        skill_effects.sort_unstable();
        let mut passive_modifiers = Vec::new();
        let mut passive_by_attribute: HashMap<i32, Vec<usize>> = HashMap::new();
        for (id, effect_id) in skill_effects {
            let Some(effect) = sde.dogma.effect(effect_id) else { continue };
            for (position, modifier) in effect.modifiers().iter().enumerate() {
                passive_by_attribute.entry(modifier.modified).or_default().push(passive_modifiers.len());
                passive_modifiers.push((Key::Skill(id), PassiveModifier::Sde { effect_id, position }));
            }
            if effect.modifiers().is_empty() {
                if let Some(modifier) = effect.name.as_deref().and_then(|name| migrated_skill_modifiers().get(name)) {
                    passive_by_attribute.entry(modifier.modified).or_default().push(passive_modifiers.len());
                    passive_modifiers.push((Key::Skill(id), PassiveModifier::Fallback(modifier)));
                }
            }
        }
        Ok(Fit { nodes, passive_modifiers, passive_by_attribute, sde, memo: Mutex::new(HashMap::new()) })
    }

    /// Visit passive skill modifiers in stable skill and effect order, including
    /// Python effects absent from the SDE modifier table.
    pub(crate) fn passive_skill_modifiers(&self, visit: impl FnMut(Key, &Modifier)) {
        self.visit_passive_modifiers(0..self.passive_modifiers.len(), visit);
    }

    fn visit_passive_modifiers(&self, indices: impl IntoIterator<Item = usize>, mut visit: impl FnMut(Key, &Modifier)) {
        for index in indices {
            let (source, handle) = &self.passive_modifiers[index];
            let modifier = match handle {
                PassiveModifier::Sde { effect_id, position } => &self.sde.dogma.effect(*effect_id).expect("indexed skill effect").modifiers()[*position],
                PassiveModifier::Fallback(modifier) => modifier,
            };
            visit(*source, modifier);
        }
    }

    pub(crate) fn required_skill(&self, source: Key, skill: Option<i32>) -> Option<i32> {
        match skill {
            Some(MODIFIER_SOURCE_ITEM_TYPE) | None => self.nodes.get(&source).map(|n| n.type_id),
            value => value,
        }
    }
    pub(crate) fn overrides(&self, key: Key) -> &[(i32, f64)] {
        self.nodes.get(&key).map_or(&[], |node| node.overrides.as_slice())
    }

    pub(crate) fn type_has_attribute(&self, key: Key, attr: i32) -> bool {
        self.nodes.get(&key).and_then(|node| node.data_position)
            .is_some_and(|position| attr == crate::consts::ATTR_CAPACITY || self.sde.types.at_position(position).attribute(attr).is_some())
    }


    pub(crate) fn base(&self, key: Key, attr: i32) -> f64 {
        let Some(node) = self.nodes.get(&key) else { return 0.0 };
        node.base(node.data_position.map(|i| self.sde.types.at_position(i)), self.sde.dogma.attribute(attr), attr)
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
            let skill = self.required_skill(source, skill);
            skill.is_some_and(|s| t.data_position.is_some_and(|i| {
                let data = self.sde.types.at_position(i);
                ATTR_REQUIRED_SKILLS.iter().filter_map(|a| data.fit_attribute(*a)).any(|v| v as i32 == s && s > 0) }))
        };
        match m.func.as_str() {
            "ItemModifier" => domain_item == Some(target),
            // Location modifiers reach what is inside the ship (modules, charges); the
            // character "location" holds its skills and implants, which no bonus targets.
            "LocationModifier" => domain_item == Some(Key::Ship) && t.in_ship,
            "LocationGroupModifier" => domain_item == Some(Key::Ship) && t.in_ship && m.group == Some(t.data_position.and_then(|i| self.sde.types.at_position(i).group_id).unwrap_or(0)),
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
        self.visit_passive_modifiers(self.passive_by_attribute.get(&attr).into_iter().flatten().copied(), |source, m| {
            if self.applies(source, m, key) {
                let v = self.value_guarded(source, m.modifying, visiting);
                mods.push((m.op, v, false));
            }
        });
        if self.sde.types.modifies_attribute(attr) {
            let stackable = self.sde.dogma.attribute(attr).and_then(|m| m.stackable).unwrap_or(true);
            for (&source, node) in &self.nodes {
                if matches!(source, Key::Skill(_)) { continue; }
                let Some(data) = node.data_position.map(|i| self.sde.types.at_position(i)) else { continue };
                for (effect, m) in data.modifiers(&self.sde.dogma, attr) {
                    let running = match effect.effect_category.unwrap_or(0) {
                        EFFECT_CATEGORY_PASSIVE => true,
                        EFFECT_CATEGORY_ONLINE => node.online,
                        EFFECT_CATEGORY_ACTIVE => node.active,
                        _ => false,
                    };
                    if !running { continue; }
                    if !self.applies(source, m, key) { continue; }
                    let v = self.value_guarded(source, m.modifying, visiting);
                    let exempt = matches!(data.category_id.unwrap_or(0), CATEGORY_SHIP | CATEGORY_CHARGE | CATEGORY_SKILL | CATEGORY_IMPLANT | CATEGORY_SUBSYSTEM);
                    mods.push((m.op, v, !stackable && !exempt));
                }
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
            let effects = node.data_position.map(|i| self.sde.types.at_position(i).effects()).unwrap_or(&[]);
            let mwd = effects.iter().any(|effect| effect.effect_id == EFFECT_MICROWARPDRIVE);
            if !node.active || !(mwd || effects.iter().any(|effect| effect.effect_id == EFFECT_AFTERBURNER)) {
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
