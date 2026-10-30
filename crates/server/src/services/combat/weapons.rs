//! Combat mechanics; state and routing live in the parent module.
use super::*;

// ---------------------------------------------------------------- weapons fire

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Weapon {
    Turret,
    Missile,
}

pub(super) fn weapon_kind(effect: &str) -> Option<Weapon> {
    match effect {
        "targetAttack" | "projectileFired" | "targetDisintegratorAttack" => Some(Weapon::Turret),
        "useMissiles" => Some(Weapon::Missile),
        _ => None,
    }
}

#[derive(Clone)]
pub(super) struct Effect {
    pub(super) id: i64,
    pub(super) guid: Option<String>,
    pub(super) duration_attribute: Option<i32>,
    pub(super) weapon: Option<Weapon>,
    /// Repairers: the layer they repair and the module attribute with the amount.
    pub(super) repair: Option<(usize, i32)>,
    /// Afterburner / microwarpdrive: the ship's speed changes while it runs.
    pub(super) propulsion: bool,
    /// Mining lasers and harvesters: the beam's effect guid; each cycle ends with a yield
    /// (services/mining).
    pub(super) mining: Option<&'static str>,
}

pub(super) fn repair_kind(effect: &str) -> Option<(usize, i32)> {
    match effect {
        "shieldBoosting" => Some((0, ATTR_SHIELD_BONUS)),
        "armorRepair" => Some((1, ATTR_ARMOR_DAMAGE_AMOUNT)),
        "structureRepair" => Some((2, ATTR_STRUCTURE_DAMAGE_AMOUNT)),
        _ => None,
    }
}

/// The ship's speed and mass as fitted now (propulsion running or not), for everyone's destiny.
pub(super) async fn apply_propulsion(cx: &Ctx, master: i64, generation: Arc<AtomicBool>, active: bool) {
    let Ok(fit) = Fit::load(&cx.db, cx.world.sde().clone(), cx.char_id, cx.ship_id).await else { return };
    let (speed, mass, signature) = (fit.value(Key::Ship, ATTR_MAX_VELOCITY), fit.value(Key::Ship, ATTR_MASS), fit.value(Key::Ship, ATTR_SIGNATURE_RADIUS));
    let ship = cx.ship_id;
    cx.world.post(cx.system_id, move |s| {
        let mut st = state();
        if !st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &generation))
            || st.active.contains_key(&master) != active { return; }
        s.command(ship, move |b, t, _| {
        b.max_vel = speed as f32;
        b.mass = mass;
        b.signature_radius = Some(signature);
        vec![
            event(t, "SetMaxSpeed", vec![Value::Int(b.id), Value::Float(speed)]),
            event(t, "SetBallMass", vec![Value::Int(b.id), Value::Float(mass)]),
        ]
        });
        if !active { st.activation_generation.remove(&master); }
    });
}

#[derive(Clone)]
pub(super) struct Module {
    pub(super) id: i64,
    pub(super) type_id: i32,
    pub(super) flag: i32,
}

/// `[self, char, ship, target, other, area, effectID, line]` (godma ENV_IDX_*).
pub(super) fn environment(cx: &Ctx, module: i64, target: Option<i64>, effect: i64) -> Value {
    Value::list([
        Value::Int(module),
        Value::Int(cx.char_id),
        Value::Int(cx.ship_id),
        target.map(Value::Int).unwrap_or(Value::None),
        Value::None,
        Value::None,
        Value::Int(effect),
        Value::None,
    ])
}

/// OnGodmaShipEffect(itemID, effectID, time, start, active, environment, startTime,
/// duration, repeat, error, actualStopTime)
#[allow(clippy::too_many_arguments)]
pub(super) fn ship_effect(cx: &Ctx, module: i64, effect: &Effect, target: Option<i64>, start: bool, started: i64, duration_ms: f64, repeat: i64) {
    let now = filetime_now();
    cx.notify(
        "OnGodmaShipEffect",
        vec![
            Value::Int(module),
            Value::Int(effect.id),
            Value::Int(now),
            Value::Int(start as i64),
            Value::Int(start as i64),
            environment(cx, module, target, effect.id),
            Value::Int(started),
            Value::Float(duration_ms),
            Value::Int(repeat),
            Value::None,
            if start { Value::None } else { Value::Int(now) },
        ],
    );
}

/// A weapon firing animation (fxSequencer.OnSpecialFX) from `shooter`.
#[allow(clippy::too_many_arguments)]
pub(super) fn fire_fx(shooter: i64, module: Option<(i64, i32)>, guid: &str, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) -> Value {
    event(
        now_tick(),
        "OnSpecialFX",
        vec![
            Value::Int(shooter),
            module.map(|m| Value::Int(m.0)).unwrap_or(Value::None),
            module.map(|m| Value::Int(m.1 as i64)).unwrap_or(Value::None),
            target.map(Value::Int).unwrap_or(Value::None),
            charge.map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
            Value::str(guid),
            Value::Bool(true),
            Value::Int(start as i64),
            Value::Int(start as i64),
            Value::Float(duration_ms),
            Value::Int(repeat),
        ],
    )
}

pub(super) fn weapon_fx(cx: &Ctx, modules: &[Module], effect: &Effect, target: Option<i64>, charge: Option<i32>, start: bool, duration_ms: f64, repeat: i64) {
    let Some(guid) = effect.guid.as_deref().and_then(|g| g.split(',').next()) else { return };
    let events = modules
        .iter()
        .map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, target, charge, start, duration_ms, repeat))
        .collect();
    cx.world.broadcast(cx.system_id, cx.ship_id, events);
}

/// Use one charge from a module; `None` when it is empty.
pub(super) async fn consume_charge(cx: &Ctx, module: &Module) -> Result<Option<i32>, CallError> {
    let row = crate::dao::items::combat::consume_charge(&cx.db, cx.ship_id, module.flag).await.map_err(db_err)?;
    let Some(r) = row else { return Ok(None) };
    let (type_id, left) = (r.type_id, r.quantity);
    if left == 0 {
        crate::dao::items::combat::delete_charge(&cx.db, cx.ship_id, module.flag).await.map_err(db_err)?;
    }
    cx.notify("OnModuleAttributeChanges", vec![quantity_change(cx.char_id, cx.ship_id, module.flag, type_id, left as i64, left as i64 + 1)]);
    Ok(Some(type_id))
}

/// The game's hit quality for the combat log: 0 miss .. 6 wrecking.
pub(super) fn hit_quality(multiplier: f64) -> i64 {
    match multiplier {
        m if m <= 0.0 => HIT_MISS,
        m if m >= HIT_WRECKING_THRESHOLD => HIT_WRECKING,
        m if m < HIT_GRAZING_THRESHOLD => HIT_GRAZING,
        m if m < HIT_GLANCING_THRESHOLD => HIT_GLANCING,
        m if m < HIT_NORMAL_THRESHOLD => HIT_NORMAL,
        m if m < HIT_GOOD_THRESHOLD => HIT_GOOD,
        _ => HIT_EXCELLENT,
    }
}

/// A turret's damage multiplier for one shot: miss (0), 0.49..1.49, or a 1% wrecking 3x,
/// with the hit chance falling off past optimal range.
pub(super) fn turret_roll(gap: f64, optimal: f64, falloff: f64) -> f64 {
    let ratio = (gap - optimal).max(0.0) / falloff.max(1.0);
    let chance = TURRET_FALLOFF_CHANCE.powf(ratio * ratio);
    let roll = rand01();
    if roll >= chance {
        0.0
    } else if roll < WRECKING_CHANCE {
        WRECKING_DAMAGE_MULTIPLIER
    } else {
        roll + NORMAL_HIT_MULTIPLIER_OFFSET
    }
}

/// `OnDamageMessage` for the shooter (`target`) or the victim (`source`).
pub(super) fn damage_message(key: &str, other: i64, weapon: i32, damage: f64, quality: i64) -> Value {
    Value::dict([
        (key, Value::Int(other)),
        ("weapon", Value::Int(weapon as i64)),
        ("damage", Value::Float(damage)),
        ("hitQuality", Value::Int(quality)),
        ("isBanked", Value::Bool(false)),
        // uilog: "me" picks AttackHits ("to {target}"), "otherPlayerWeapons" AttackHitsR
        // ("from {source}").
        ("attackType", Value::str(if key == "source" { "otherPlayerWeapons" } else { "me" })),
        ("damageTypes", Value::Dict(vec![])),
    ])
}

/// Tell both sides of a hit: the shooter (if a pilot) and the victim (if a pilot).
pub(super) fn report_hit(world: &World, shooter: i64, target: i64, shooter_pilot: Option<i64>, target_pilot: Option<i64>, weapon: i32, damage: f64, quality: i64) {
    if let Some(ch) = shooter_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("target", target, weapon, damage, quality)]);
    }
    if let Some(ch) = target_pilot {
        world.notify_pilot(ch, "OnDamageMessage", vec![damage_message("source", shooter, weapon, damage, quality)]);
    }
}

/// One turret shot at `target`, with the fitted module's and its charge's effective attributes.
pub(super) async fn fire_turret(cx: &Ctx, fit: &Fit, module: &Module, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.ball(ship_id)?.clone(), s.ball(target)?.clone()))).await.flatten() else { return Ok(()) };
    let (m, ch) = (Key::Module(module.id), Key::Charge(module.id));
    let gap = (dist(ship.pos, t.pos) - t.radius as f64 - ship.radius as f64).max(0.0);
    let multiplier = turret_roll(gap, fit.value(m, ATTR_MAX_RANGE), fit.value(m, ATTR_FALLOFF));
    let base = fit.value(m, ATTR_DAMAGE_MULTIPLIER).max(0.0) * multiplier;
    let raw = ATTR_DAMAGE.map(|id| fit.value(ch, id) * base);
    let dealt = if multiplier > 0.0 { damage(&cx.db, &cx.world, cx.system_id, target, t.char_id, raw, Some(cx.char_id)).await? } else { 0.0 };
    report_hit(&cx.world, cx.ship_id, target, ship.char_id, t.char_id, module.type_id, dealt, hit_quality(multiplier));
    Ok(())
}

/// What a missile does when it arrives.
pub(super) struct Missile {
    pub(super) charge: i32,
    pub(super) raw: [f64; 4],
    pub(super) aoe_cloud: f64,
    pub(super) velocity: f64,
    pub(super) flight_range: f64,
    pub(super) weapon: i32,
    /// The pilot who fired it.
    pub(super) attacker: Option<i64>,
}

/// Launch a missile ball from `from` at `target`: it flies client-side; on impact it
/// explodes and deals damage scaled by explosion radius vs. the target's signature.
pub(super) async fn launch_missile(db: Db, world: Arc<World>, system: i64, from: Ball, target: Ball, owner: i64, module: Option<i64>, m: Missile) -> Result<(), CallError> {
    let size = db
        .query_one("SELECT COALESCE(radius, 1)::float8 AS radius, COALESCE(mass, 1)::float8 AS mass FROM sde.inv_types WHERE type_id = $1", &[&m.charge])
        .await
        .map_err(db_err)?;
    let size = SizeRow::from_row(&size);
    let mut slim = vec![("sourceShipID", Value::Int(from.id))];
    slim.push(("launchModules", Value::list(module.map(Value::Int))));
    let missile = Ball {
        id: NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed),
        type_id: m.charge,
        owner_id: owner,
        corp_id: from.corp_id,
        alliance_id: -1,
        char_id: None,
        signature_radius: None,
        mode: MODE_MISSILE,
        radius: size.radius as f32,
        mass: size.mass,
        max_vel: m.velocity as f32,
        agility: 1.0,
        speed_fraction: 1.0,
        pos: from.pos,
        vel: [0.0; 3],
        goto: target.pos,
        follow_id: target.id,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: false,
        warp: None,
        warp_factor: 0,
        last_ms: now_ms(),
        cloak: 0,
        cloak_until: 0,
        health: [1.0; 3],
        launcher: from.id,
        slim,
    };
    let missile_id = missile.id;
    world.add_ship(system, missile).await.map_err(CallError::Internal)?;
    let signature = attr(world.sde(), target.type_id, ATTR_SIGNATURE_RADIUS, target.radius as f64);
    let distance = dist(from.pos, target.pos);
    let delay_ms = seconds_to_ms((distance / m.velocity.max(1.0)).clamp(MIN_MISSILE_FLIGHT_SECONDS, MAX_MISSILE_FLIGHT_SECONDS));
    let (shooter_id, shooter_pilot) = (from.id, from.char_id);
    let (target_id, target_pilot) = (target.id, target.char_id);
    let (flight_range, aoe_cloud, raw, weapon, attacker) = (m.flight_range, m.aoe_cloud, m.raw, m.weapon, m.attacker);
    let world_ref = Arc::downgrade(&world);
    world.schedule(system, delay_ms, move |s| {
        s.destroy_ball(missile_id);
        let present = s.contains(target_id);
        let reached = distance <= flight_range + MISSILE_REACH_ALLOWANCE_METRES;
        s.defer(async move {
            let Some(world) = world_ref.upgrade() else { return };
            let (dealt, quality) = if reached && present {
                let application = (signature / aoe_cloud.max(1.0)).min(1.0);
                (damage(&db, &world, system, target_id, target_pilot, raw.map(|d| d * application), attacker).await.unwrap_or(0.0), HIT_NORMAL)
            } else {
                (0.0, HIT_MISS)
            };
            report_hit(&world, shooter_id, target_id, shooter_pilot, target_pilot, weapon, dealt, quality);
        });
    });
    Ok(())
}

pub(super) async fn fire_missile(cx: &Ctx, fit: &Fit, module: &Module, charge: i32, target: i64) -> Result<(), CallError> {
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.ball(ship_id)?.clone(), s.ball(target)?.clone()))).await.flatten() else { return Ok(()) };
    let ch = Key::Charge(module.id);
    let velocity = fit.value(ch, ATTR_MAX_VELOCITY).max(1.0);
    let missile = Missile {
        charge,
        raw: ATTR_DAMAGE.map(|id| fit.value(ch, id) * fit.value(Key::Char, ATTR_MISSILE_DAMAGE_MULTIPLIER).max(0.0)),
        aoe_cloud: fit.value(ch, ATTR_AOE_CLOUD_SIZE),
        velocity,
        flight_range: velocity * ms_to_seconds(fit.value(ch, ATTR_EXPLOSION_DELAY)),
        weapon: module.type_id,
        attacker: Some(cx.char_id),
    };
    launch_missile(cx.db.clone(), cx.world.clone(), cx.system_id, ship, t, cx.char_id, Some(module.id), missile).await
}

/// Owns the markers for one activation, even while a callback or worker is queued.
pub(super) struct ActivationOwner {
    pub(super) master: i64,
    pub(super) generation: Arc<AtomicBool>,
    pub(super) armed: bool,
}

impl Drop for ActivationOwner {
    fn drop(&mut self) {
        if !self.armed { return; }
        let mut st = state();
        if st.active.get(&self.master).is_some_and(|current| Arc::ptr_eq(&current.stop, &self.generation)) {
            st.active.remove(&self.master);
        }
        if st.activation_generation.get(&self.master).is_some_and(|current| Arc::ptr_eq(current, &self.generation)) {
            st.activation_generation.remove(&self.master);
        }
    }
}

/// One activation moves between actor callbacks and its deferred phases.
pub(super) struct Cycle {
    pub(super) cx: Ctx,
    pub(super) modules: Vec<Module>,
    pub(super) effect: Effect,
    pub(super) target: Option<i64>,
    pub(super) repeat: i64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) owner: ActivationOwner,
    pub(super) started: i64,
    pub(super) charge_type: Option<i32>,
    pub(super) cycles: i64,
    pub(super) duration_ms: f64,
    pub(super) nominal_start: i64,
}

pub(super) fn cycle_valid(s: &System, cycle: &Cycle) -> bool {
    s.contains(cycle.cx.ship_id)
        && cycle.target.is_none_or(|target| s.contains(target))
        && state().active.get(&cycle.modules[0].id).is_some_and(|v| Arc::ptr_eq(&v.stop, &cycle.stop))
}

pub(super) fn cycle_deadline(start: i64, duration_ms: f64) -> i64 {
    start.saturating_add(seconds_to_ms(ms_to_seconds(duration_ms)) as i64)
}

pub(super) fn cycle_is_final(stopped: bool, repeat: i64, cycles: i64) -> bool {
    stopped || repeat <= 0 || cycles >= repeat
}

pub(super) fn cycle_should_stop(finished: bool, stop: &AtomicBool) -> bool {
    finished || stop.load(Ordering::Relaxed)
}

pub(super) fn schedule_cycle_end(s: &mut System, start: i64, duration_ms: f64, f: impl FnOnce(&mut System, i64) + Send + 'static) {
    let end = cycle_deadline(start, duration_ms);
    s.schedule_at(end, move |s| f(s, end));
}

pub(super) fn retire_activation(active: &mut HashMap<i64, ActiveModule>, master: i64, stop: &Arc<AtomicBool>, publish: impl FnOnce()) -> bool {
    if !active.get(&master).is_some_and(|current| Arc::ptr_eq(&current.stop, stop)) { return false; }
    publish();
    active.remove(&master);
    true
}


pub(super) fn cycle_start(s: &mut System, mut cycle: Cycle, nominal_start: i64) {
    if !cycle_valid(s, &cycle) {
        cycle_stop(s, cycle);
        return;
    }
    cycle.nominal_start = nominal_start;
    s.defer(async move { cycle_begin_work(cycle).await; });
}

pub(super) async fn cycle_begin_work(mut cycle: Cycle) {
    let nominal_start = cycle.nominal_start;
    let Cycle { cx, modules, effect, target, repeat, stop, charge_type, cycles, duration_ms, .. } = &mut cycle;
    let master = modules[0].id;
    if *cycles == 0 && effect.propulsion {
        apply_propulsion(cx, master, stop.clone(), true).await;
    }
    let reason = 'work: {
        // Effective attributes as they are now (charges change, modules go offline...).
        let fit = match Fit::load(&cx.db, cx.world.sde().clone(), cx.char_id, cx.ship_id).await {
            Ok(f) => f,
            Err(_) => {
                break 'work "fit unavailable";
            }
        };
        *duration_ms = effect.duration_attribute.map(|a| fit.value(Key::Module(master), a)).unwrap_or(*duration_ms).max(MIN_MODULE_DURATION_MS);
        let need: f64 = modules.iter().map(|m| fit.value(Key::Module(m.id), ATTR_CAPACITOR_NEED)).sum();
        if !drain_capacitor(&cx, &fit, need) {
            cx.notify("OnEffectMessage", vec![Value::str("NotEnoughCharge"), Value::Dict(vec![])]);
            break 'work "out of capacitor";
        }
        if let Some(kind) = effect.weapon {
            let target = target.unwrap_or(0);
            if !targets_of(cx.char_id).contains(&target) {
                break 'work "target lost";
            }
            let mut fired = false;
            for m in modules.iter() {
                let charge = match consume_charge(&cx, m).await {
                    Ok(Some(t)) => t,
                    _ => continue,
                };
                *charge_type = Some(charge);
                fired = true;
                let r = match kind {
                    Weapon::Turret => fire_turret(&cx, &fit, m, target).await,
                    Weapon::Missile => fire_missile(&cx, &fit, m, charge, target).await,
                };
                if let Err(e) = r {
                    warn!("weapon {} failed: {}", m.id, match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                }
            }
            if !fired {
                break 'work "out of charges";
            }
            if *cycles == 0 {
                weapon_fx(cx, modules, effect, Some(target), *charge_type, true, *duration_ms, *repeat);
            }
        }
        if let (Some(guid), Some(rock)) = (effect.mining, *target) {
            if !targets_of(cx.char_id).contains(&rock) {
                break 'work "target lost";
            }
            if *cycles == 0 {
                let beam = modules.iter().map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, Some(rock), None, true, *duration_ms, *repeat)).collect();
                cx.world.broadcast(cx.system_id, cx.ship_id, beam);
            }
        }
        // Shield boosters land at the start of a cycle, armor and hull repairers at its end.
        let repair = effect.repair.map(|(layer, a)| (layer, fit.value(Key::Module(master), a)));
        if let Some((0, amount)) = repair {
            let _ = repair_player(&cx.db, &cx.world, cx.system_id, cx.ship_id, cx.char_id, 0, amount).await;
        }
        *cycles += 1;
        let duration = *duration_ms;
        let world = cx.world.clone();
        let system = cx.system_id;
        world.post(system, move |s| {
            schedule_cycle_end(s, nominal_start, duration, move |s, end| cycle_end(s, cycle, fit, repair, end));
        });
        return;
    };
    debug!("module {master} stopped: {reason}");
    let world = cycle.cx.world.clone();
    world.post(cycle.cx.system_id, move |s| cycle_stop(s, cycle));
}

pub(super) fn cycle_end(s: &mut System, cycle: Cycle, fit: Fit, repair: Option<(usize, f64)>, end: i64) {
    if !cycle_valid(s, &cycle) {
        cycle_stop(s, cycle);
        return;
    }
    let final_cycle = cycle_is_final(cycle.stop.load(Ordering::Relaxed), cycle.repeat, cycle.cycles);
    s.defer(async move {
        let cx = &cycle.cx;
        let master = cycle.modules[0].id;
        if let Some((layer @ (1 | 2), amount)) = repair {
            let _ = repair_player(&cx.db, &cx.world, cx.system_id, cx.ship_id, cx.char_id, layer, amount).await;
        }
        let mut finished = final_cycle;
        if let (Some(_), Some(rock)) = (cycle.effect.mining, cycle.target) {
            match super::super::mining::extract(&cx.db, &cx.world, &cx.notify, cx.char_id, cx.ship_id, cx.system_id, &fit, master, rock).await {
                Ok(true) => {}
                Ok(false) => finished = true,
                Err(e) => {
                    warn!("mining {master}: {}", match e {
                        CallError::Internal(e) => format!("{e:#}"),
                        CallError::User { msg, .. } => msg,
                    });
                    finished = true;
                }
            }
        }
        let world = cx.world.clone();
        world.post(cx.system_id, move |s| {
            if cycle_should_stop(finished, &cycle.stop) {
                cycle_stop(s, cycle);
            } else {
                cycle_start(s, cycle, end);
            }
        });
    });
}

pub(super) fn cycle_stop(s: &mut System, cycle: Cycle) {
    let Cycle { cx, modules, effect, target, repeat, stop, mut owner, started, charge_type, cycles, duration_ms, .. } = cycle;
    let master = modules[0].id;
    debug!("module {master} stopped after {cycles} cycles");
    let mut st = state();
    let retired = retire_activation(&mut st.active, master, &stop, || {
        ship_effect(&cx, master, &effect, target, false, started, duration_ms, repeat);
        if effect.weapon.is_some() {
            if let Some(guid) = effect.guid.as_deref().and_then(|g| g.split(',').next()) {
                let events = modules.iter().map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, target, charge_type, false, duration_ms, repeat)).collect();
                s.broadcast(cx.ship_id, events);
            }
        }
        if let Some(guid) = effect.mining {
            let events = modules.iter().map(|m| fire_fx(cx.ship_id, Some((m.id, m.type_id)), guid, target, None, false, duration_ms, repeat)).collect();
            s.broadcast(cx.ship_id, events);
        }
    });
    if retired && !effect.propulsion && st.activation_generation.get(&master).is_some_and(|current| Arc::ptr_eq(current, &stop)) {
        st.activation_generation.remove(&master);
    }
    if retired { owner.armed = false; }
    drop(st);
    if retired && effect.propulsion {
        s.defer(async move { apply_propulsion(&cx, master, stop, false).await; });
    }
}

/// Activate(itemID, effectName, targetID, repeat)
pub(super) async fn activate(c: &mut Client, call: &Call<'_>) -> CallResult {
    let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let effect_name = call.args.get(1).and_then(|v| v.as_text()).map(|s| s.into_owned()).unwrap_or_default();
    let target = call.args.get(2).and_then(|v| v.as_int());
    let repeat = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0);
    let Some(cx) = ctx(c) else { return Ok(Value::None) };
    let Some(module) = load_items(&c.db, ItemQuery::IdAt(item_id, cx.ship_id)).await.map_err(db_err)?.into_iter().next() else {
        return Ok(Value::None);
    };
    let row = cx
        .db
        .query_opt("SELECT effect_id, guid, duration_attribute_id FROM sde.dogma_effects WHERE name = $1", &[&effect_name])
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown effect {effect_name}")))?;
    let row = EffectRow::from_row(&row);
    let effect = Effect {
        id: row.effect_id as i64,
        guid: row.guid.filter(|g| !g.is_empty()),
        duration_attribute: row.duration_attribute_id,
        weapon: weapon_kind(&effect_name),
        repair: repair_kind(&effect_name),
        propulsion: matches!(row.effect_id, EFFECT_AFTERBURNER | EFFECT_MICROWARPDRIVE),
        mining: match effect_name.as_str() {
            "miningLaser" => Some("effects.Mining"),
            "miningClouds" => Some("effects.CloudMining"),
            _ => None,
        },
    };
    if state().reloading.get(&item_id).is_some_and(|until| *until > filetime_now()) {
        return Err(user_error(messages::MSG_CANT_ACTIVATE_RELOADING_MODULE, vec![("moduleTypeID".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
    }
    if effect.weapon.is_some() {
        let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
            return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
        };
        let ship_id = cx.ship_id;
        let (in_warp, own_type, present, victim) = cx.world.with(cx.system_id, move |s| {
            let ship = s.ball(ship_id);
            let target = s.ball(t);
            (ship.is_some_and(|b| b.mode == MODE_WARP), ship.map(|b| b.type_id).unwrap_or(0), target.is_some(), target.and_then(|b| b.char_id))
        }).await.unwrap_or((false, 0, false, None));
        if in_warp {
            return Err(user_error(messages::MSG_DENIED_ACTIVATE_IN_WARP, vec![]));
        }
        if !present {
            return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
        }
        if loaded(&cx.db, cx.ship_id, module.flag_id).await?.is_none_or(|(_, q)| q <= 0) {
            return Err(user_error(messages::MSG_NO_CHARGES, vec![("launcher".into(), Value::tuple([Value::Int(UE_TYPEID), Value::Int(module.type_id as i64)]))]));
        }
        // Shooting another pilot is a matter for crimewatch.
        if let Some(victim) = victim {
            super::super::crimewatch::aggress(&cx.db, &cx.world, cx.system_id, cx.char_id, own_type, module.type_id, victim, t).await?;
        }
    }
    if effect.mining.is_some() {
        let Some(t) = target.filter(|t| targets_of(cx.char_id).contains(t)) else {
            return Err(user_error(messages::MSG_DENIED_ACTIVATE_TARGET_NOT_PRESENT, vec![]));
        };
        if !super::super::mining::is_rock(t) {
            return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::MINING_MODULE_TARGET_INVALID))]));
        }
        let ship_id = cx.ship_id;
        let gap = cx.world.with(cx.system_id, move |s| {
            s.ball(ship_id).zip(s.ball(t)).map(|(a, b)| dist(a.pos, b.pos) - a.radius as f64 - b.radius as f64)
        }).await.flatten();
        let fit = Fit::load(&cx.db, cx.world.sde().clone(), cx.char_id, cx.ship_id).await?;
        let range = fit.value(Key::Module(item_id), ATTR_MAX_RANGE);
        if gap.is_none_or(|g| g > range) {
            return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::mining_range(metres_to_km(range))))]));
        }
    }
    if state().active.contains_key(&item_id) {
        return Ok(Value::None);
    }
    let fit = Fit::load(&cx.db, cx.world.sde().clone(), cx.char_id, cx.ship_id).await?;
    let duration_ms = effect.duration_attribute.map(|a| fit.value(Key::Module(item_id), a)).unwrap_or(DEFAULT_MODULE_DURATION_MS).max(MIN_MODULE_DURATION_MS);
    // The group fires together.
    let mut modules = vec![Module { id: item_id, type_id: module.type_id, flag: module.flag_id }];
    if effect.weapon.is_some() {
        for slave in bank(&cx.db, cx.ship_id, item_id).await?.into_iter().skip(1) {
            if let Some(m) = load_items(&c.db, ItemQuery::IdAt(slave, cx.ship_id)).await.map_err(db_err)?.into_iter().next() {
                modules.push(Module { id: m.item_id, type_id: m.type_id, flag: m.flag_id });
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        if st.active.contains_key(&item_id) { return Ok(Value::None); }
        st.active.insert(item_id, ActiveModule { char_id: cx.char_id, stop: stop.clone() });
        st.activation_generation.insert(item_id, stop.clone());
    }
    let owner = ActivationOwner { master: item_id, generation: stop.clone(), armed: true };
    ship_effect(&cx, item_id, &effect, target, true, filetime_now(), duration_ms, repeat);
    info!(user = c.login.user_id, "module {item_id} activated {effect_name} on {target:?} (x{}, {duration_ms:.0} ms, repeat {repeat})", modules.len());
    let world = cx.world.clone();
    let cycle = Cycle { cx, modules, effect, target, repeat, stop, owner, started: filetime_now(), charge_type: None, cycles: 0, duration_ms, nominal_start: 0 };
    world.post(cycle.cx.system_id, move |s| {
        let now = s.now_ms();
        cycle_start(s, cycle, now);
    });
    Ok(Value::None)
}

/// Put a fitted module online or offline in space: remember it, and tell godma (the online
/// effect's state drives the HUD button) and the client's dogma (isOnline).
pub(super) async fn set_online(c: &mut Client, item_id: i64, online: bool) -> CallResult {
    let Some(cx) = ctx(c) else { return Ok(Value::None) };
    let changed = crate::dao::items::combat::set_online(&cx.db, item_id, cx.ship_id, online).await.map_err(db_err)?;
    if changed == 0 {
        return Ok(Value::None);
    }
    if !online {
        if let Some(stop) = state().active.get(&item_id) {
            stop.stop.store(true, Ordering::Relaxed);
        }
    }
    let effect = Effect { id: EFFECT_ONLINE, guid: None, duration_attribute: None, weapon: None, repair: None, propulsion: false, mining: None };
    ship_effect(&cx, item_id, &effect, None, online, filetime_now(), -1.0, -1);
    let change = attribute_change(cx.char_id, Value::Int(item_id), ATTR_IS_ONLINE, Value::Int(online as i64), Value::Int(!online as i64));
    cx.notify("OnModuleAttributeChanges", vec![Value::list([change])]);
    info!(user = c.login.user_id, "module {item_id} {}", if online { "online" } else { "offline" });
    Ok(Value::None)
}

/// Show a module running one cycle (analyzers): start now, stop after `duration_ms`.


/// Deactivate(itemID, effectName): stop after the current cycle.
pub(super) fn deactivate(call: &Call<'_>) -> CallResult {
    let item_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    if let Some(stop) = state().active.get(&item_id) {
        stop.stop.store(true, Ordering::Relaxed);
    }
    Ok(Value::None)
}

