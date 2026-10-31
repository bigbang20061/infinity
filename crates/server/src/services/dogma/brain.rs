//! Active skill, implant and booster modifiers and hull overrides for dogmaIM and OnServerBrainUpdated.
//! Mirrors eve/client/script/dogma/allSkillsCharBrain.py and
//! eve/common/script/dogma/effect.py, using Fit's SDE and active character material.
//! Arithmetic operations through PostAssign and PostPercentUnNerfed are compiled;
//! the special skill relationship is derived by Fit, not emitted. Structure effects
//! and implant/booster injection or expiry are not supported. Versions are in-memory;
//! training-completion updates occur when skill handlers collect completed training.

use super::super::{fit::{Fit, Key, OP_PRE_ASSIGN, OP_PRE_MUL, OP_PRE_DIV, OP_MOD_ADD, OP_MOD_SUB, OP_POST_MUL, OP_POST_DIV, OP_POST_PERCENT, OP_POST_PERCENT_UNNERFED, OP_POST_ASSIGN, PERCENT_SCALE}, keyval, CallError};
use crate::{client::Client, consts::ATTR_SKILL_LEVEL};
use marshal::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};


static VERSIONS: OnceLock<Mutex<HashMap<i64, i64>>> = OnceLock::new();
fn versions() -> &'static Mutex<HashMap<i64, i64>> {
    VERSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Target {
    ship: bool,
    attribute: i32,
    kind: &'static str,
    operation: i32,
    extra: Option<i32>,
}

struct Effect {
    value: f64,
    skills: Vec<i32>,
}
fn combine(effects: &mut BTreeMap<Target, Effect>, target: Target, value: f64, skill: i32) {
    // Division by zero is ignored by Fit; zero additions and percentages are neutral.
    if value == 0.0 && matches!(target.operation, OP_PRE_DIV | OP_POST_DIV | OP_MOD_ADD | OP_MOD_SUB | OP_POST_PERCENT | OP_POST_PERCENT_UNNERFED) { return; }
    if let Some(effect) = effects.get_mut(&target) {
        effect.value = match target.operation {
            OP_PRE_ASSIGN | OP_POST_ASSIGN => value,
            OP_PRE_MUL | OP_PRE_DIV | OP_POST_MUL | OP_POST_DIV => effect.value * value,
            OP_MOD_ADD | OP_MOD_SUB => effect.value + value,
            OP_POST_PERCENT | OP_POST_PERCENT_UNNERFED => (PERCENT_SCALE + effect.value) / PERCENT_SCALE * ((PERCENT_SCALE + value) / PERCENT_SCALE) * PERCENT_SCALE - PERCENT_SCALE,
            _ => unreachable!(),
        };
        effect.skills.push(skill);
    } else {
        effects.insert(target, Effect { value, skills: vec![skill] });
    }
}

fn insert_hull_overrides(effects: &mut BTreeMap<Target, Effect>, overrides: &[(i32, f64)], present: impl Fn(i32) -> bool) {
    for &(attribute, value) in overrides {
        if present(attribute) {
            effects.insert(Target { ship: true, attribute, kind: "M", operation: OP_PRE_ASSIGN, extra: None }, Effect { value, skills: Vec::new() });
        }
    }
}

fn wire(target: &Target, effect: Effect, to_item: i64) -> Value {
    let extras = Value::tuple(target.extra.into_iter().map(|id| Value::Int(id as i64)));
    // The constructor reads fromAttrib.invItem.typeID before state is restored.
    let from = keyval([("invItem", keyval([("typeID", Value::Int(effect.skills.first().copied().unwrap_or(0) as i64))]))]);
    let state = Value::dict([
        ("fromAttrib", Value::None),
        ("value", Value::Float(effect.value)),
        ("toItemID", Value::Int(to_item)),
        ("modifierType", Value::str(target.kind)),
        ("toAttribID", Value::Int(target.attribute as i64)),
        ("operation", Value::Int(target.operation as i64)),
        ("extras", extras.clone()),
        ("skills", Value::list(effect.skills.into_iter().map(|id| Value::Int(id as i64)))),
    ]);
    Value::reduce("eve.common.script.dogma.effect.BrainEffect", vec![from, Value::Int(to_item), Value::str(target.kind), Value::Int(target.attribute as i64), Value::Int(target.operation as i64), extras], Some(state))
}


/// Compile from the same effective source values and modifier traversal as Fit.
pub(crate) fn compile(fit: &Fit, char_id: i64, ship_id: i64) -> (Value, Value, Value) {
    let mut effects: BTreeMap<Target, Effect> = BTreeMap::new();
    insert_hull_overrides(&mut effects, fit.overrides(Key::Ship), |attribute| fit.type_has_attribute(Key::Ship, attribute));
    fit.brain_modifiers(|source, modifier| {
        let Some(source_type) = fit.source_type(source) else { return };
        let ship = match modifier.domain.as_str() {
            "shipID" => true,
            "charID" => false,
            _ => return,
        };
        let (kind, extra) = match modifier.func.as_str() {
            "ItemModifier" => ("M", None),
            "LocationModifier" => ("L", None),
            "LocationGroupModifier" => ("LG", modifier.group),
            "LocationRequiredSkillModifier" => ("LRS", fit.required_skill(source, modifier.skill)),
            "OwnerRequiredSkillModifier" => ("ORS", fit.required_skill(source, modifier.skill)),
            _ => return,
        };
        if matches!(kind, "LG" | "LRS" | "ORS") && extra.is_none() { return; }
        if !(OP_PRE_ASSIGN..=OP_POST_ASSIGN).contains(&modifier.op) && modifier.op != OP_POST_PERCENT_UNNERFED { return; }
        let value = fit.value(source, modifier.modifying);
        if kind == "M" && modifier.modified == ATTR_SKILL_LEVEL { return; }
        let target = Target { ship, attribute: modifier.modified, kind, operation: modifier.op, extra };
        combine(&mut effects, target, value, source_type);
    });
    let mut char_effects = Vec::new();
    let mut ship_effects = Vec::new();
    for (target, effect) in effects {
        let to_item = if target.ship { ship_id } else { char_id };
        let wire = wire(&target, effect, to_item);
        if target.ship { ship_effects.push(wire); } else { char_effects.push(wire); }
    }
    (Value::List(char_effects), Value::List(ship_effects), Value::list([]))
}

async fn effects(c: &Client) -> Result<(i64, Value, Value, Value), CallError> {
    let char_id = c.session.int("charid").ok_or_else(|| CallError::Internal(anyhow::anyhow!("no character selected")))?;
    let ship_id = c.session.int("shipid").unwrap_or(0);
    let fit = if let Some(system) = c.session.int("solarsystemid") {
        Fit::load_in_space(&c.db, &c.world, system, char_id, ship_id).await?
    } else {
        Fit::load(&c.db, c.sde.clone(), char_id, ship_id).await?
    };
    let (character, ship, structure) = compile(&fit, char_id, ship_id);
    Ok((char_id, character, ship, structure))
}

pub(crate) async fn bootstrap(c: &Client) -> Result<Value, CallError> {
    let (char_id, character, ship, structure) = effects(c).await?;
    let version = *versions().lock().unwrap().entry(char_id).or_insert(0);
    Ok(Value::tuple([Value::Int(version), character, ship, structure]))
}

/// Call after a committed skill change; sends one brainData tuple to the client.
pub(crate) async fn update(c: &mut Client) -> Result<(), CallError> {
    let (char_id, character, ship, structure) = effects(c).await?;
    let version = {
        let mut versions = versions().lock().unwrap();
        let version = versions.entry(char_id).or_insert(0);
        *version += 1;
        *version
    };
    let bytes = marshal::dumps(&Value::tuple([character, ship, structure]));
    c.notify("OnServerBrainUpdated", vec![Value::tuple([Value::Int(version), Value::Bytes(bytes)])]);
    Ok(())
}

#[cfg(test)] mod tests;
