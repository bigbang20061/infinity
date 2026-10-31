//! GM per-item dogma base overrides. The slash role gate is owned by the parent service.
//! Mirrors godma item attributes and the client brain's hull preAssignment effects;
//! overrides live in game.item_attributes and follow singleton item identity.
//! Charges, characters, structures, NPCs and stacked items are not supported.
use super::{slash_error, Client, CallError};
use crate::{dao::items::{dogma as dao, fit as fit_dao, loading::{Item, ItemQuery}}, messages};
use crate::consts::{CATEGORY_CHARGE, CATEGORY_ENTITY, CATEGORY_MODULE, CATEGORY_OWNER};
use crate::services::{db_err, fit::{self, Fit, Key}, items::load_items, combat};
use marshal::Value;

/// SDE invCategories: Starbase.
const CATEGORY_STARBASE: i64 = 23;
/// SDE invCategories: Infrastructure Upgrade.
const CATEGORY_INFRASTRUCTURE_UPGRADE: i64 = 39;
/// SDE invCategories: Sovereignty Structures.
const CATEGORY_SOVEREIGNTY_STRUCTURES: i64 = 40;
/// SDE invCategories: Structure.
const CATEGORY_STRUCTURE: i64 = 65;
/// SDE invCategories: Structure Module.
const CATEGORY_STRUCTURE_MODULE: i64 = 66;
/// Categories that are not override targets.
const EXCLUDED_CATEGORIES: [i64; 8] = [
    CATEGORY_OWNER as i64, CATEGORY_CHARGE as i64, CATEGORY_ENTITY as i64,
    CATEGORY_STARBASE, CATEGORY_INFRASTRUCTURE_UPGRADE, CATEGORY_SOVEREIGNTY_STRUCTURES,
    CATEGORY_STRUCTURE, CATEGORY_STRUCTURE_MODULE,
];
/// SDE dgmAttributeTypes: the five primary character attributes.
const CHARACTER_ATTRIBUTES: std::ops::RangeInclusive<i32> = 164..=168;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Target { Ship, Item(i64), Flag(i32) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Action<'a> { List, Query(&'a str), Reset(&'a str), Set(&'a str, f64) }

pub(super) fn parse<'a>(args: &[&'a str]) -> Option<(Target, Action<'a>)> {
    let target = match *args.first()? {
        "ship" => Target::Ship,
        s if s.starts_with("flag:") => Target::Flag(s[5..].parse::<i32>().ok().filter(|v| *v >= 0)?),
        s => Target::Item(s.parse::<i64>().ok().filter(|v| *v > 0)?),
    };
    let action = match &args[1..] {
        ["list"] => Action::List,
        [attr] if !attr.is_empty() => Action::Query(attr),
        [attr, "reset"] if !attr.is_empty() && *attr != "list" => Action::Reset(attr),
        [attr, value] if !attr.is_empty() && *attr != "list" => {
            let value = value.parse::<f64>().ok().filter(|v| v.is_finite())?;
            Action::Set(attr, value)
        }
        _ => return None,
    };
    Some((target, action))
}

fn attribute(c: &Client, name: &str) -> Result<i32, CallError> {
    if let Ok(id) = name.parse::<i32>() {
        return c.sde.dogma.attribute(id).map(|_| id).ok_or_else(|| slash_error(messages::gm_dogma_attribute(name)));
    }
    let mut ids: Vec<_> = c.sde.dogma.attributes_by_name(name).map(|a| a.attribute_id).collect();
    ids.sort_unstable();
    match ids.as_slice() {
        [id] => Ok(*id),
        [] => Err(slash_error(messages::gm_dogma_attribute(name))),
        _ => Err(slash_error(messages::gm_dogma_ambiguous(name, &ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")))),
    }
}

pub(super) fn denied(id: i32, matches_runtime: impl Fn(&str, i32) -> bool) -> Option<&'static str> {
    if CHARACTER_ATTRIBUTES.contains(&id) { return Some(messages::GM_DOGMA_CHARACTER); }
    let runtime = ["isOnline", "damage", "charge", "skillPoints", "skillLevel", "armorDamage", "shieldCharge", "isIncapacitated", "quantity"];
    runtime.iter().any(|name| matches_runtime(name, id)).then_some(messages::GM_DOGMA_RUNTIME)
}

async fn item(c: &Client, id: i64) -> Result<Item, CallError> {
    load_items(&c.db, &c.sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next()
        .ok_or_else(|| slash_error(messages::gm_dogma_invalid_target(&id.to_string())))
}

pub(super) async fn command(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let (target, action) = parse(args).ok_or_else(|| slash_error(messages::GM_DOGMA_USAGE.into()))?;
    let ship = c.session.int("shipid");
    let modules = if let Some(ship) = ship { fit_dao::modules(&c.db, ship).await.map_err(db_err)? } else { Vec::new() };
    let id = match target {
        Target::Ship => ship.ok_or_else(|| slash_error(messages::GM_DOGMA_NO_SHIP.into()))?,
        Target::Item(id) => id,
        Target::Flag(flag) => modules.iter().find(|m| m.flag_id == flag).map(|m| m.item_id)
            .ok_or_else(|| slash_error(messages::gm_dogma_invalid_target(&flag.to_string())))?,
    };
    let item = item(c, id).await?;
    if EXCLUDED_CATEGORIES.contains(&item.category_id) {
        return Err(slash_error(messages::GM_DOGMA_FORBIDDEN_TARGET.into()));
    }
    if !item.singleton { return Err(slash_error(messages::GM_DOGMA_SINGLETON.into())); }
    if action == Action::List {
        let mut items = vec![item];
        if matches!(target, Target::Ship) {
            for module in &modules { items.push(self::item(c, module.item_id).await?); }
        }
        let ids: Vec<_> = items.iter().map(|i| i.item_id).collect();
        let rows = dao::overrides_for(&c.db, &ids).await.map_err(db_err)?;
        let mut lines = Vec::new();
        for i in &items {
            let name = c.sde.types.get(i.type_id).and_then(|t| t.name.as_deref()).unwrap_or("");
            lines.push(messages::gm_dogma_list_item(i.flag_id, name, i.item_id));
            let mut found = false;
            for row in rows.iter().filter(|r| r.item_id == i.item_id) {
                found = true;
                lines.push(messages::gm_dogma_list_entry(row.attribute_id, row.value));
            }
            if !found { lines.push(messages::GM_DOGMA_EMPTY.into()); }
        }
        return Ok(lines.join("\n"));
    }
    let attr_name = match action { Action::Query(a) | Action::Reset(a) | Action::Set(a, _) => a, Action::List => unreachable!() };
    let attr = attribute(c, attr_name)?;
    if let Some(reason) = denied(attr, |name, id| c.sde.dogma.attributes_by_name(name).any(|a| a.attribute_id == id)) {
        return Err(slash_error(reason.into()));
    }
    let key = if Some(id) == ship { Some(Key::Ship) } else if modules.iter().any(|m| m.item_id == id) { Some(Key::Module(id)) } else { None };
    if !matches!(action, Action::Query(_)) {
        let cycle = ["duration", "speed", "capacitorNeed", "reloadTime"];
        if combat::active_modules().contains(&id)
            && cycle.iter().any(|name| c.sde.dogma.attributes_by_name(name).any(|a| a.attribute_id == attr)) {
            return Err(slash_error(messages::GM_DOGMA_ACTIVE_CYCLE.into()));
        }
    }
    let rows = dao::overrides(&c.db, id).await.map_err(db_err)?;
    let old_override = rows.iter().find(|r| r.attribute_id == attr).map(|r| r.value);
    if matches!(action, Action::Reset(_)) && old_override.is_none() { return Ok(messages::gm_dogma_noop(id, attr)); }
    let fallback = fit::base_attribute(c.sde.types.get(item.type_id), c.sde.dogma.attribute(attr), attr);
    let char_id = c.session.int("charid").unwrap_or(0);
    let old_fit = if key.is_some() { Some(Fit::load(&c.db, c.sde.clone(), char_id, ship.unwrap()).await?) } else { None };
    let old_base = old_fit.as_ref().zip(key).map(|(fit, key)| fit.base(key, attr)).unwrap_or(old_override.unwrap_or(fallback));
    let old_effective = old_fit.as_ref().zip(key).map(|(fit, key)| fit.value(key, attr));
    match action {
        Action::Set(_, value) => {
            if !dao::set_override(&c.db, id, attr, value).await.map_err(db_err)? {
                return Err(slash_error(messages::GM_DOGMA_SINGLETON.into()));
            }
        }
        Action::Reset(_) => dao::clear_override(&c.db, id, attr).await.map_err(db_err)?,
        Action::Query(_) => {},
        Action::List => unreachable!(),
    }
    let changed = !matches!(action, Action::Query(_));
    let new_fit = if changed && key.is_some() { Some(Fit::load(&c.db, c.sde.clone(), char_id, ship.unwrap()).await?) } else { None };
    let current_fit = new_fit.as_ref().or(old_fit.as_ref());
    let new_base = current_fit.zip(key).map(|(fit, key)| fit.base(key, attr)).unwrap_or(match action { Action::Set(_, value) => value, Action::Reset(_) => fallback, _ => old_base });
    let new_effective = current_fit.zip(key).map(|(fit, key)| fit.value(key, attr));
    if changed {
        if let (Some(old), Some(new), Some(key)) = (old_fit.as_ref(), new_fit.as_ref(), key) {
            let before = combat::weapons::kinematics(old);
            let after = combat::weapons::kinematics(new);
            if before != after && c.session.int("stationid").is_none() {
                if let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), ship) {
                    c.world.post(system, move |s| combat::weapons::queue_kinematics(s, ship, after));
                }
            }
            let entry = combat::groups_health::attribute_change(char_id, Value::Int(id), attr, Value::Float(new.value(key, attr)), Value::Float(old.value(key, attr)));
            c.world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::list([entry])]);
            if key == Key::Ship && new.type_has_attribute(key, attr) { crate::services::dogma::brain::update(c).await?; }
        }
    }
    let overridden = match action { Action::Set(..) => true, Action::Reset(_) => false, _ => old_override.is_some() };
    let mut result = messages::gm_dogma_result(id, attr_name, attr, old_base, new_base, old_effective.zip(new_effective), overridden);
    if key.is_none() { result.push_str("\n"); result.push_str(messages::GM_DOGMA_BASE_ONLY); }
    let hull_foreign = item.category_id == i64::from(crate::consts::CATEGORY_SHIP)
        && attr != crate::consts::ATTR_CAPACITY
        && c.sde.types.get(item.type_id).is_none_or(|data| data.attribute(attr).is_none());
    if item.category_id == i64::from(CATEGORY_MODULE) || hull_foreign {
        result.push_str("\n"); result.push_str(messages::GM_DOGMA_VISIBILITY);
    }
    Ok(result)
}
