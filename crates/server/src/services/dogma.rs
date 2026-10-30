//! dogmaIM (bound per location): item attribute/effect state for godma and
//! clientDogmaLocation (eve/client/script/environment/godma.py ProcessAllInfo).

use super::{db_err, item_descriptor, keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, short, Client};
use crate::consts::{ATTR_ARMOR_DAMAGE, ATTR_CAPACITOR_CAPACITY, ATTR_CAPACITY, ATTR_CHARGE, ATTR_HULL_DAMAGE, ATTR_IS_ONLINE, ATTR_MASS, ATTR_QUANTITY, ATTR_RADIUS, ATTR_SHIELD_CAPACITY, ATTR_SHIELD_CHARGE, ATTR_VOLUME, BLUEPRINT_COPY_QUANTITY, EFFECT_ONLINE, FLAG_HANGAR, FLAG_PILOT, SINGLETON_QUANTITY, TYPE_CHARACTER};
use marshal::Value;
use std::collections::BTreeMap;
use tracing::warn;

/// SDE invGroups: Character.
const GROUP_CHARACTER: i64 = 1;
/// SDE invCategories: Owner.
const CATEGORY_OWNER: i64 = 1;

/// SDE dgmAttributeTypes: HUD defaults for attributes 1175 through 1177.
const HUD_ATTRIBUTE_1175: i64 = 1175;
/// SDE dgmAttributeTypes: HUD default attribute 1176.
const HUD_ATTRIBUTE_1176: i64 = 1176;
/// SDE dgmAttributeTypes: HUD default attribute 1177.
const HUD_ATTRIBUTE_1177: i64 = 1177;
/// Server online-effect duration wire choice for godma.py RefreshItemEffects' tuple.
const ONLINE_EFFECT_DURATION: i64 = -1;
/// Server online-effect repeat wire choice for godma.py RefreshItemEffects' tuple.
const ONLINE_EFFECT_REPEAT: i64 = -1;
/// Server brain version wire choice for godma.py ProcessAllInfo's character brain tuple.
const BRAIN_VERSION: i64 = -1;

/// Inventory wire quantity: singleton originals are -1 and blueprint copies are -2.
pub(super) fn wire_quantity(singleton: bool, quantity: i64) -> i64 {
    if singleton { if quantity == BLUEPRINT_COPY_QUANTITY { BLUEPRINT_COPY_QUANTITY } else { SINGLETON_QUANTITY } } else { quantity }
}

/// An inventory item as the client's item DBRow expects it.
#[derive(Clone)]
pub struct Item {
    pub item_id: i64,
    pub type_id: i32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i32,
    pub quantity: i32,
    pub singleton: bool,
    pub group_id: i64,
    pub category_id: i64,
    pub custom_info: Option<String>,
}

impl Item {
    pub fn row(&self) -> Value {
        let quantity = wire_quantity(self.singleton, self.quantity as i64);
        item_descriptor().row(&[
            Value::Int(self.item_id),
            Value::Int(self.type_id as i64),
            Value::Int(self.owner_id),
            Value::Int(self.location_id),
            Value::Int(self.flag_id as i64),
            Value::Int(quantity),
            Value::Int(self.group_id),
            Value::Int(self.category_id),
            self.custom_info.as_ref().map(Value::str).unwrap_or(Value::None),
        ])
    }
}

/// Items by `item_id = ANY(ids)` joined to their group/category.
pub async fn load_items(c: &Client, where_sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Vec<Item>, CallError> {
    load_items_db(&c.db, where_sql, params).await
}

/// load_items for tasks without a client.
pub async fn load_items_db(db: &(impl crate::db::executor::Executor + ?Sized), where_sql: &str, params: &[&(dyn tokio_postgres::types::ToSql + Sync)]) -> Result<Vec<Item>, CallError> {
    let rows = db
        .query(
            &format!(
                "SELECT i.item_id, i.type_id, i.owner_id, i.location_id, i.flag_id, i.quantity, i.singleton,
                        t.group_id::bigint, t.category_id::bigint, i.custom_info
                 FROM game.items i JOIN sde.inv_types t USING (type_id) WHERE {where_sql} ORDER BY i.item_id"
            ),
            params,
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| Item {
            item_id: r.get(0),
            type_id: r.get(1),
            owner_id: r.get(2),
            location_id: r.get(3),
            flag_id: r.get(4),
            quantity: r.get(5),
            singleton: r.get(6),
            group_id: r.get::<_, Option<i64>>(7).unwrap_or(0),
            category_id: r.get::<_, Option<i64>>(8).unwrap_or(0),
            custom_info: r.get(9),
        })
        .collect())
}

async fn type_attributes(c: &Client, type_id: i32) -> Result<BTreeMap<i64, f64>, CallError> {
    let rows = c
        .db
        .query("SELECT attribute_id, value FROM sde.type_attributes WHERE type_id = $1", &[&type_id])
        .await
        .map_err(db_err)?;
    let mut attrs: BTreeMap<i64, f64> = rows.iter().map(|r| (r.get::<_, i32>(0) as i64, r.get::<_, f64>(1))).collect();
    // mass, capacity, volume and radius are type columns, not dogma attribute rows.
    let columns = c
        .db
        .query_opt("SELECT mass::float8, capacity::float8, volume::float8, radius::float8 FROM sde.inv_types WHERE type_id = $1", &[&type_id])
        .await
        .map_err(db_err)?;
    if let Some(r) = columns {
        for (i, attr) in [ATTR_MASS, ATTR_CAPACITY, ATTR_VOLUME, ATTR_RADIUS].into_iter().enumerate() {
            if let Some(v) = r.get::<_, Option<f64>>(i) {
                attrs.entry(attr as i64).or_insert(v);
            }
        }
    }
    Ok(attrs)
}

/// godma's activeEffects for an online module: {effectID: (itemID, charID, shipID, targetID,
/// otherID, area, effectID, startTime, duration, repeat)}. godma counts an effect as running
/// only when it is listed here (RefreshItemEffects), so an empty dict shows every module offline.
fn online_effect(item_id: i64, char_id: i64, ship_id: i64) -> Vec<(Value, Value)> {
    let env = [
        Value::Int(item_id),
        Value::Int(char_id),
        Value::Int(ship_id),
        Value::None,
        Value::None,
        Value::list([]),
        Value::Int(EFFECT_ONLINE),
        Value::Int(filetime_now()),
        Value::Int(ONLINE_EFFECT_DURATION),
        Value::Int(ONLINE_EFFECT_REPEAT),
    ];
    vec![(Value::Int(EFFECT_ONLINE), Value::tuple(env))]
}

fn info(item_row: Value, item_id: i64, attributes: BTreeMap<i64, f64>, active: Vec<(Value, Value)>) -> Value {
    info_keyed(item_row, Value::Int(item_id), attributes, active)
}

/// `info` for any item key: an itemID, or a charge sublocation's (shipID, flagID, typeID).
fn info_keyed(item_row: Value, key: Value, attributes: BTreeMap<i64, f64>, active: Vec<(Value, Value)>) -> Value {
    let now = filetime_now();
    keyval([
        ("itemID", key),
        ("invItem", item_row),
        ("attributes", Value::Dict(attributes.into_iter().map(|(k, v)| (Value::Int(k), Value::Float(v))).collect())),
        ("activeEffects", Value::Dict(active)),
        ("time", Value::Int(now)),
        ("wallclockTime", Value::Int(now)),
    ])
}

/// The character as a dogma item, located in its ship.
async fn character_info(c: &Client) -> Result<Value, CallError> {
    let char_id = c.session.int("charid").ok_or_else(|| CallError::Internal(anyhow::anyhow!("no character selected")))?;
    let ship_id = c.session.int("shipid").unwrap_or(0);
    let character_attrs = super::skills::character_attributes(c, char_id).await?;
    let mut attrs = type_attributes(c, TYPE_CHARACTER).await?;
    attrs.extend(character_attrs);
    let item = Item {
        item_id: char_id,
        type_id: TYPE_CHARACTER,
        owner_id: char_id,
        location_id: ship_id,
        flag_id: FLAG_PILOT,
        quantity: 1,
        singleton: true,
        group_id: GROUP_CHARACTER,
        category_id: CATEGORY_OWNER,
        custom_info: None,
    };
    Ok(info(item.row(), char_id, attrs, vec![]))
}

/// Fitted modules of `ship_id` that are online (game.items.online).
async fn online_modules(c: &Client, ship_id: i64) -> Result<Vec<i64>, CallError> {
    Ok(c.db
        .query("SELECT item_id FROM game.items WHERE location_id = $1 AND online", &[&ship_id])
        .await
        .map_err(db_err)?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

/// `(instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates)` for a ship being made
/// active (clientDogmaLocation.MakeShipActive). instanceCache rows follow dgmAttributesByIdx:
/// [itemID, isOnline, damage, charge, skillPoints, armorDamage, shieldCharge, isIncapacitated].
pub async fn ship_state(c: &Client, ship_id: i64) -> Result<Value, CallError> {
    let items = load_items(c, "i.location_id = $1 AND i.flag_id <> $2", &[&ship_id, &FLAG_HANGAR]).await?;
    let online = online_modules(c, ship_id).await?;
    let instance = |key: Value, online: bool| {
        let row = [
            key.clone(),
            Value::Int(online as i64),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Int(0),
        ];
        (key, Value::list(row))
    };
    let mut instances: Vec<(Value, Value)> = items.iter().map(|i| instance(Value::Int(i.item_id), online.contains(&i.item_id))).collect();
    // Loaded charges: sublocations (shipID, flagID, typeID) with their quantities.
    let (quantities, charges) = super::combat::flag_quantities(c, ship_id).await?;
    instances.extend(charges.into_iter().map(|k| instance(k, false)));
    let banks = super::combat::banks(&c.db, ship_id).await?;
    Ok(Value::tuple([Value::Dict(instances), quantities, banks, Value::Dict(vec![])]))
}

/// The active ship and everything fitted in it.
async fn ship_info(c: &Client) -> Result<Vec<(Value, Value)>, CallError> {
    let Some(ship_id) = c.session.int("shipid") else { return Ok(vec![]) };
    let items = load_items(c, "i.item_id = $1 OR (i.location_id = $1 AND i.flag_id <> $2)", &[&ship_id, &FLAG_HANGAR]).await?;
    let online = online_modules(c, ship_id).await?;
    let mut out = Vec::new();
    // Runtime state the HUD reads: a full capacitor (with skills), and the ship's shield,
    // armor and structure as combat left them.
    super::combat::refill_capacitor(ship_id);
    let char_id = c.session.int("charid").unwrap_or(0);
    let fit = super::fit::Fit::load(&c.db, char_id, ship_id).await?;
    let (cap, shield_max) = (fit.value(super::fit::Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(super::fit::Key::Ship, ATTR_SHIELD_CAPACITY));
    let (shield, armor_damage, hull_damage) = super::combat::ship_condition(ship_id).unwrap_or((shield_max, 0.0, 0.0));
    for item in items {
        let mut attrs = type_attributes(c, item.type_id).await?;
        // godma shows attributes as given, so the ship and its modules carry their fitted
        // values (skills, modules, rigs): the HUD's shield/armor/hull maxima, cap, speed...
        let key = if item.item_id == ship_id {
            Some(super::fit::Key::Ship)
        } else if item.location_id == ship_id {
            Some(super::fit::Key::Module(item.item_id))
        } else {
            None
        };
        if let Some(key) = key {
            for (attr, value) in attrs.iter_mut() {
                *value = fit.value(key, *attr as i32);
            }
        }
        let active = if online.contains(&item.item_id) {
            attrs.insert(ATTR_IS_ONLINE as i64, 1.0);
            online_effect(item.item_id, char_id, ship_id)
        } else {
            vec![]
        };
        attrs.entry(ATTR_MASS as i64).or_insert(0.0); // mass is always present
        if item.item_id == ship_id {
            attrs.insert(ATTR_CHARGE as i64, cap);
            attrs.insert(ATTR_SHIELD_CHARGE as i64, shield);
            attrs.insert(ATTR_ARMOR_DAMAGE as i64, armor_damage);
            attrs.insert(ATTR_HULL_DAMAGE as i64, hull_damage);
        }
        for k in [ATTR_HULL_DAMAGE as i64, ATTR_ARMOR_DAMAGE as i64, HUD_ATTRIBUTE_1175, HUD_ATTRIBUTE_1176, HUD_ATTRIBUTE_1177] {
            attrs.entry(k).or_insert(0.0);
        }
        out.push((Value::Int(item.item_id), info(item.row(), item.item_id, attrs, active)));
    }
    // Loaded charges are sublocations, not items: godma (UpdateItem) files an entry keyed
    // (shipID, flagID, typeID) with no invItem under the ship, with its quantity attribute;
    // the HUD reads a launcher's charge from there.
    let charges = c
        .db
        .query("SELECT flag_id, type_id, quantity FROM game.module_charges WHERE ship_id = $1 AND quantity > 0", &[&ship_id])
        .await
        .map_err(db_err)?;
    for r in charges {
        let (flag, type_id, qty): (i32, i32, i32) = (r.get(0), r.get(1), r.get(2));
        let mut attrs = type_attributes(c, type_id).await?;
        attrs.insert(ATTR_QUANTITY as i64, qty as f64);
        let key = Value::tuple([Value::Int(ship_id), Value::Int(flag as i64), Value::Int(type_id as i64)]);
        out.push((key.clone(), info_keyed(Value::None, key, attrs, vec![])));
    }
    Ok(out)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    // Targeting, ammo, weapon groups and module activation.
    if let Some(r) = super::combat::dispatch(c, call).await {
        return r;
    }
    let flag = |i: usize| call.args.get(i).map(|v| !v.is_none() && v.as_int() != Some(0)).unwrap_or(false);
    match call.method {
        // GetAllInfo(getCharInfo, getShipInfo, structureID)
        "GetAllInfo" => {
            let char_info = if flag(0) {
                let ch = character_info(c).await?;
                let char_id = c.session.int("charid").unwrap_or(0);
                // (charItems, brain); brain = (version, charEffects, shipEffects, structureEffects)
                // -- godma.Prime unpacks "_, charBrain = allInfo.charInfo".
                let brain = Value::tuple([Value::Int(BRAIN_VERSION), Value::list([]), Value::list([]), Value::list([])]);
                Value::tuple([Value::Dict(vec![(Value::Int(char_id), ch)]), brain])
            } else {
                Value::None
            };
            let ship_info = if flag(1) { Value::Dict(ship_info(c).await?) } else { Value::None };
            Ok(keyval([
                ("charInfo", char_info),
                ("shipInfo", ship_info),
                ("structureInfo", Value::None),
                ("shipModifiedCharAttribs", Value::None),
                ("systemWideEffectsOnShip", Value::None),
                // (instanceCache, instanceFlagQuantityCache, weaponBanks, heatStates) for MakeShipActive.
                ("shipState", match c.session.int("shipid") {
                    Some(ship) => ship_state(c, ship).await?,
                    None => Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![])]),
                }),
            ]))
        }
        "ShipGetInfo" => Ok(Value::Dict(ship_info(c).await?)),
        "CharGetInfo" => character_info(c).await,
        "ItemGetInfo" => {
            let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let items = load_items(c, "i.item_id = $1", &[&item_id]).await?;
            match items.into_iter().next() {
                Some(item) => {
                    let attrs = type_attributes(c, item.type_id).await?;
                    let online = online_modules(c, item.location_id).await?.contains(&item.item_id);
                    let char_id = c.session.int("charid").unwrap_or(0);
                    let active = if online { online_effect(item.item_id, char_id, item.location_id) } else { vec![] };
                    Ok(info(item.row(), item.item_id, attrs, active))
                }
                None if Some(item_id) == c.session.int("charid") => character_info(c).await,
                None => Ok(Value::None),
            }
        }
        "GetCharacterBaseAttributes" => {
            let ch = character_info(c).await?;
            Ok(ch)
        }
        "CheckSendLocationInfo" | "LogAttribute" => Ok(Value::None),
        "LaunchProbes" => super::exploration::launch_probes(c, call).await,
        // SetModuleOnline(shipID, moduleID) / TakeModuleOffline(shipID, moduleID): the client
        // runs the online effect itself (fitting checks included); we remember the state.
        "SetModuleOnline" | "TakeModuleOffline" => {
            let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            let module = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
            let online = call.method == "SetModuleOnline";
            c.db
                .execute("UPDATE game.items SET online = $3 WHERE item_id = $1 AND location_id = $2", &[&module, &ship, &online])
                .await
                .map_err(db_err)?;
            Ok(Value::None)
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED dogmaIM::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
