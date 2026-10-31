//! Combat mechanics; state and routing live in the parent module.
use super::*;
use crate::consts::CATEGORY_MODULE;

// ---------------------------------------------------------------- weapon groups

/// `{masterID: [slaveIDs]}` for a ship.


pub(super) async fn link(db: &(impl Executor + ?Sized), ship: i64, master: i64, slave: i64) -> Result<(), CallError> {
    if master == slave {
        return Ok(());
    }
    // A master joining another group brings its own slaves along.
    crate::dao::items::combat::link(db, ship, master, slave).await.map_err(db_err)?;
    Ok(())
}

pub(super) async fn weapon_groups(c: &mut Client, call: &Call<'_>) -> CallResult {
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let ship = int(0);
    let db = c.db.clone();
    let method = match call.method {
        "LinkWeapons" => "LinkWeapons",
        "MergeModuleGroups" => "MergeModuleGroups",
        "PeelAndLink" => "PeelAndLink",
        "LinkAllWeapons" => "LinkAllWeapons",
        "UnlinkModule" => "UnlinkModule",
        "UnlinkAllModules" => "UnlinkAllModules",
        _ => "",
    };
    let (master, other) = (int(1), int(2));
    let fallback_ship = if ship != 0 { ship } else { c.session.int("shipid").unwrap_or(0) };
    db.transaction(
        |db, _outbox: &mut Outbox| Box::pin(async move {
    match method {
        "LinkWeapons" | "MergeModuleGroups" | "PeelAndLink" => {
            if method == "PeelAndLink" {
                crate::dao::items::combat::peel_and_link(db, ship, master, other).await.map_err(db_err)?;
            } else {
                link(db, ship, master, other).await?;
            }
            banks(db, ship).await
        }
        // Group every online high-slot module with the others of its type.
        "LinkAllWeapons" => {
            crate::dao::items::combat::clear_banks(db, ship).await.map_err(db_err)?;
            let rows = crate::dao::items::combat::fitted_weapons(db, ship, *HI_SLOTS.start(), *HI_SLOTS.end()).await.map_err(db_err)?;
            let mut masters: HashMap<i32, i64> = HashMap::new();
            for r in rows {
                let (id, type_id) = (r.item_id, r.type_id);
                match masters.get(&type_id) {
                    Some(&m) => link(db, ship, m, id).await?,
                    None => {
                        masters.insert(type_id, id);
                    }
                }
            }
            banks(db, ship).await
        }
        // Takes the last module off a group; returns it.
        "UnlinkModule" => {
            let slave = crate::dao::items::combat::unlink_last(db, ship, master).await.map_err(db_err)?;
            Ok(slave.map(Value::Int).unwrap_or(Value::None))
        }
        "UnlinkAllModules" => {
            crate::dao::items::combat::clear_banks(db, ship).await.map_err(db_err)?;
            Ok(Value::None)
        }
        _ => banks(db, fallback_ship).await,
    }
        }),
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}

// ---------------------------------------------------------------- capacitor and hit points

/// Capacitor or shield after `dt` seconds of passive recharge from `level` (the game's
/// curve: recharge peaks at 25%, ~98% after one recharge time `tau_ms`).
pub(super) fn recharge(level: f64, max: f64, tau_ms: f64, dt: f64) -> f64 {
    if max <= 0.0 {
        return 0.0;
    }
    let tau = (tau_ms / 1000.0).max(0.001);
    let c = 1.0 + ((level / max).clamp(0.0, 1.0).sqrt() - 1.0) * (-5.0 * dt / tau).exp();
    (max * c * c).min(max)
}

/// One `OnModuleAttributeChanges` entry.
pub(crate) fn attribute_change(owner: i64, item: Value, attribute: i32, new: Value, old: Value) -> Value {
    Value::tuple([
        Value::str("OnModuleAttributeChange"),
        Value::Int(owner),
        item,
        Value::Int(attribute as i64),
        Value::Int(filetime_now()),
        new,
        old,
        Value::None,
    ])
}

/// Use `amount` of the ship's capacitor; false (and nothing used) when there isn't enough.
pub(super) fn drain_capacitor(cx: &Ctx, fit: &Fit, amount: f64) -> bool {
    if amount <= 0.0 {
        return true;
    }
    let (max, tau) = (fit.value(Key::Ship, ATTR_CAPACITOR_CAPACITY), fit.value(Key::Ship, ATTR_RECHARGE_RATE));
    let now = filetime_now();
    let current = {
        let mut st = state();
        let (level, at) = st.capacitor.get(&cx.ship_id).copied().unwrap_or((max, now));
        let current = recharge(level, max, tau, (now - at) as f64 / FILETIME_SECOND as f64);
        if current < amount {
            return false;
        }
        st.capacitor.insert(cx.ship_id, (current - amount, now));
        current
    };
    let change = attribute_change(cx.char_id, Value::Int(cx.ship_id), ATTR_CHARGE, Value::Float(current - amount), Value::Float(current));
    cx.notify("OnModuleAttributeChanges", vec![Value::list([change])]);
    true
}

/// A ship's hit points and resistances, from its fit.
#[derive(Clone)]
pub(super) struct ShipStats {
    pub(super) max: [f64; 3],
    pub(super) resonance: [[f64; 4]; 3],
    pub(super) shield_tau_ms: f64,
}

/// A player ship's condition: shield (recharging since `shield_at`), armor and structure left.
#[derive(Clone)]
pub(super) struct Condition {
    pub(super) stats: ShipStats,
    pub(super) now: [f64; 3],
    pub(super) shield_at: i64,
}

pub(super) async fn ship_stats(db: &Db, world: &World, char_id: i64, ship: i64) -> Result<ShipStats, CallError> {
    let fit = Fit::load(db, world.sde().clone(), char_id, ship).await?;
    Ok(ShipStats {
        max: ATTR_HP.map(|a| fit.value(Key::Ship, a)),
        resonance: ATTR_RESONANCE.map(|layer| layer.map(|a| fit.value(Key::Ship, a))),
        shield_tau_ms: fit.value(Key::Ship, ATTR_SHIELD_RECHARGE_RATE),
    })
}

/// (shieldCharge, armorDamage, damage) of a ship that has taken damage since it was last
/// repaired, with its shield recharged to now; None for an undamaged ship.


/// Docking repairs the ship.


/// A ship being (re)loaded by its pilot (undocking, boarding) starts with a full capacitor.


/// Forget a destroyed ship.


/// Take raw damage (em, thermal, kinetic, explosive) through layers with resonances;
/// returns the damage dealt.
pub(super) fn absorb(now: &mut [f64; 3], resonance: &[[f64; 4]; 3], raw: [f64; 4]) -> f64 {
    let mut left = raw;
    let mut dealt = 0.0;
    for layer in 0..3 {
        let effective: f64 = (0..4).map(|i| left[i] * resonance[layer][i]).sum();
        if effective <= 0.0 {
            break;
        }
        if effective <= now[layer] {
            now[layer] -= effective;
            dealt += effective;
            break;
        }
        // This layer breaks; what it didn't absorb carries on to the next.
        let used = now[layer] / effective;
        dealt += now[layer];
        now[layer] = 0.0;
        left = left.map(|d| d * (1.0 - used));
    }
    dealt
}

/// Damage an NPC (or any non-player ball); destroys it at zero structure.
pub(super) async fn damage_npc(db: &Db, world: &World, system: i64, target: i64, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    let Some(ball) = world.ball(system, target).await else { return Ok(0.0) };
    if let Some(pilot) = attacker {
        world.post(system, move |s| {
            s.add_aggressor(target, pilot);
        });
    }
    let known = state().hp.get(&target).cloned();
    let mut hp = match known {
        Some(hp) => hp,
        None => {
            let max = ATTR_HP.map(|id| attr(world.sde(), ball.type_id, id, 0.0));
            Hp { now: max, max, resonance: ATTR_RESONANCE.map(|layer| layer.map(|id| attr(world.sde(), ball.type_id, id, 1.0))) }
        }
    };
    let dealt = absorb(&mut hp.now, &hp.resonance, raw);
    if hp.now[2] <= 0.0 && (hp.max[2] > 0.0 || hp.now.iter().all(|v| *v <= 0.0)) {
        let destroyed = world.destroy_ball(system, target).await;
        forget_target(world, target);
        if let Some(ball) = destroyed {
            info!("ball {target} (type {}) destroyed", ball.type_id);
            if let Err(e) = reward_kill(db, world, system, &ball, attacker).await {
                warn!("wreck of {target}: {}", match e {
                    CallError::Internal(e) => format!("{e:#}"),
                    CallError::User { msg, .. } => msg,
                });
            }
        }
    } else {
        let health = [0, 1, 2].map(|i| if hp.max[i] > 0.0 { (hp.now[i] / hp.max[i]).clamp(0.0, 1.0) } else { 0.0 });
        state().hp.insert(target, hp);
        world.set_health(system, target, health);
    }
    Ok(dealt)
}

/// Damage a player's ship; at zero structure it explodes and its pilot is told to eject
/// (`__ship_destroyed`, handled by the pilot's connection).
pub(super) async fn damage_player(db: &Db, world: &World, system: i64, ship: i64, char_id: i64, raw: [f64; 4], attacker: Option<i64>) -> Result<f64, CallError> {
    // Resistances and hit points as fitted right now (hardeners running, modules offline...).
    let stats = ship_stats(db, world, char_id, ship).await?;
    let known = state().condition.get(&ship).cloned();
    let mut cond = match known {
        Some(mut c) => {
            c.now = [0, 1, 2].map(|i| c.now[i].min(stats.max[i]));
            c.stats = stats;
            c
        }
        None => Condition { now: stats.max, stats, shield_at: filetime_now() },
    };
    let now = filetime_now();
    let old = cond.now;
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    let dealt = absorb(&mut cond.now, &cond.stats.resonance, raw);
    if let Some(attacker) = attacker {
        super::super::crimewatch::note_damage(ship, attacker, dealt);
    }
    let max = cond.stats.max;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(max[2] - old[2])),
    ];
    world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    if cond.now[2] <= 0.0 {
        info!("ship {ship} of {char_id} destroyed");
        destroy_player_ship(world, system, ship, char_id).await;
    } else {
        let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
        state().condition.insert(ship, cond);
        world.set_health(system, ship, health);
    }
    Ok(dealt)
}

/// A player's ship explodes: its pilot is told to eject (`__ship_destroyed`, handled by
/// the pilot's connection). Also how CONCORD ends a criminal.


/// Damage whatever `target` is: a player's ship or an NPC. `attacker` is the pilot
/// responsible, if any (NPCs remember them; kills pay them).
pub(super) async fn damage(db: &Db, world: &World, system: i64, target: i64, target_pilot: Option<i64>, raw: [f64; DAMAGE_TYPE_COUNT], attacker: Option<i64>) -> Result<f64, CallError> {
    match target_pilot {
        Some(char_id) => damage_player(db, world, system, target, char_id, raw, attacker).await,
        None => damage_npc(db, world, system, target, raw, attacker).await,
    }
}

/// Repair a player's ship layer (0 shield, 1 armor, 2 structure) by `amount`.
pub(super) async fn repair_player(db: &Db, world: &World, system: i64, ship: i64, char_id: i64, layer: usize, amount: f64) -> Result<(), CallError> {
    let known = state().condition.get(&ship).cloned();
    // An undamaged ship has nothing to repair.
    let Some(mut cond) = known else { return Ok(()) };
    let now = filetime_now();
    let old = cond.now;
    cond.now[0] = recharge(cond.now[0], cond.stats.max[0], cond.stats.shield_tau_ms, (now - cond.shield_at) as f64 / FILETIME_SECOND as f64);
    cond.shield_at = now;
    cond.now[layer] = (cond.now[layer] + amount).min(cond.stats.max[layer]);
    let max = cond.stats.max;
    let changes = vec![
        attribute_change(char_id, Value::Int(ship), ATTR_SHIELD_CHARGE, Value::Float(cond.now[0]), Value::Float(old[0])),
        attribute_change(char_id, Value::Int(ship), ATTR_ARMOR_DAMAGE, Value::Float(max[1] - cond.now[1]), Value::Float(max[1] - old[1])),
        attribute_change(char_id, Value::Int(ship), ATTR_HULL_DAMAGE, Value::Float(max[2] - cond.now[2]), Value::Float(max[2] - old[2])),
    ];
    world.notify_pilot(char_id, "OnModuleAttributeChanges", vec![Value::List(changes)]);
    let health = [0, 1, 2].map(|i| if max[i] > 0.0 { (cond.now[i] / max[i]).clamp(0.0, 1.0) } else { 0.0 });
    state().condition.insert(ship, cond);
    world.set_health(system, ship, health);
    let _ = db;
    Ok(())
}

/// A destroyed NPC pays its bounty to the pilot who killed it and leaves a wreck (its
/// group's wreck type, sde_native.wrecks) holding loot worth about its bounty.
pub(super) async fn reward_kill(db: &Db, world: &World, system: i64, npc: &Ball, killer: Option<i64>) -> Result<(), CallError> {
    let bounty = attr(world.sde(), npc.type_id, ATTR_ENTITY_KILL_BOUNTY, 0.0);
    if let Some(pilot) = killer.filter(|_| bounty > 0.0) {
        // A player corporation takes its tax rate of the bounty into its master wallet.
        let corp_tax = crate::dao::characters::combat::corporation_tax(db, pilot).await.map_err(db_err)?
            .map(|r| (r.corporation_id, r.tax_rate.clamp(0.0, 1.0)));
        let tax = corp_tax.map(|(_, rate)| super::super::market::round2(bounty * rate)).unwrap_or(0.0);
        let balance = crate::dao::wallets::characters::credit(db, pilot, bounty - tax).await.map_err(db_err)?;
        world.notify_pilot(pilot, "OnAccountChange", vec![Value::str("cash"), Value::Int(pilot), Value::Float(balance)]);
        if let Some((corp, _)) = corp_tax.filter(|_| tax > 0.0) {
            let mut outbox = Outbox::default();
            super::super::wallets::corp_credit(db, &mut outbox, corp, ACCOUNT_CASH, tax).await?;
            dispatch_world((), outbox, world)?;
        }
        info!("{pilot} earned {bounty:.0} ISK bounty for {} ({tax:.0} corporation tax)", npc.type_id);
        super::super::crimewatch::npc_killed(db, world, pilot).await?;
    }
    let sde = world.sde();
    let Some(npc_type) = sde.types.get(npc.type_id) else { return Ok(()) };
    let Some(wreck_type) = npc_type.group_id.and_then(|group| sde.wrecks.get(group, npc_type.race_id.unwrap_or(0))) else { return Ok(()) };
    let radius = sde.types.get(wreck_type).and_then(|t| t.radius).unwrap_or(100.0);
    let owner = killer.unwrap_or(npc.owner_id);
    let wreck_id = crate::dao::items::creation::positioned(db, wreck_type, owner, system, npc.pos).await.map_err(db_err)?;
    // Loot: one or two modules or ammo stacks that traded on Serenity, worth up to the bounty.
    let picks = 1 + (rand01() * 2.0) as i64;
    let mut loot: Vec<_> = crate::dao::items::combat::loot(db, bounty).await.map_err(db_err)?.into_iter()
        .filter_map(|r| sde.types.get(r.type_id).and_then(|t| loot_category(t.published, t.category_id).map(|category| (r.type_id, category))))
        .collect();
    for i in 0..(picks as usize).min(loot.len()) {
        let j = i + (rand01() * (loot.len() - i) as f64) as usize;
        loot.swap(i, j);
    }
    loot.truncate(picks as usize);
    for &(type_id, category) in &loot {
        let quantity: i32 = if category == CATEGORY_CHARGE { 20 + (rand01() * 100.0) as i32 } else { 1 };
        crate::dao::items::combat::insert_loot(db, type_id, owner, wreck_id, quantity).await.map_err(db_err)?;
    }
    let ball = Ball {
        id: wreck_id,
        type_id: wreck_type,
        owner_id: owner,
        corp_id: npc.corp_id,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: crate::space::MODE_RIGID,
        radius: radius as f32,
        mass: 10_000.0,
        max_vel: 0.0,
        agility: 1.0,
        speed_fraction: 0.0,
        pos: npc.pos,
        vel: [0.0; 3],
        old_pos: npc.pos, old_vel: [0.0; 3], segment_target: None, segment_warp: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: false,
        global: false,
        massive: true,
        interactive: false,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: 0,
        slim: vec![("isEmpty", Value::Bool(loot.is_empty()))],
    };
    world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    debug!("wreck {wreck_id} ({wreck_type}) with {} loot stacks", loot.len());
    Ok(())
}

fn loot_category(published: Option<bool>, category: Option<i32>) -> Option<i32> {
    category.filter(|&id| published == Some(true) && (id == CATEGORY_MODULE || id == CATEGORY_CHARGE))
}

#[cfg(test)] mod loot_tests;



