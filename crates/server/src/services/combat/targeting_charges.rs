//! Combat targeting and ammunition loading, mirroring eve/client/script/dogma/clientDogmaLocation.py.
//! Uses game.items, game.module_charges and game.pending_reloads for inventory and reservations.
//! Manual and empty-magazine ancillary reloads share SDE charge compatibility,
//! reservations and actor deadlines that do not precede the advertised FILETIME.
//! Automatic reload searches only ship cargo.
//! Reload timer markers are memory-only; newer starts return older reservations, and startup recovery returns pending charges rather than resuming timers.
use super::*;

// ---------------------------------------------------------------- targeting

pub(crate) fn targets_of(char_id: i64) -> Vec<i64> {
    state().targets.get(&char_id).map(|t| t.iter().copied().collect()).unwrap_or_default()
}

/// AddTarget(targetID) -> (lockPending, targets). The lock completes after the lock
/// time from the ship's scan resolution and the target's signature radius, with
/// `OnTarget('add', targetID)`.
pub(super) async fn add_target(c: &mut Client, target: i64) -> CallResult {
    let Some(cx) = ctx(c) else { return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![])) };
    let ship_id = cx.ship_id;
    let Some((ship, t)) = cx.world.with(cx.system_id, move |s| Some((s.sampled_ball(ship_id)?, s.sampled_ball(target)?))).await.flatten() else {
        return Err(user_error(messages::MSG_TARGETING_ATTEMPT_CANCELLED, vec![]));
    };
    let fit = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?;
    let range = fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE);
    let gap = dist(ship.pos, t.pos) - t.radius as f64;
    if gap > range {
        let text = messages::target_lock_range(metres_to_km(gap), metres_to_km(range));
        return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(text))]));
    }
    // The ship's limit, capped by the pilot's (Targeting skills).
    let pilot_limit = fit.value(Key::Char, ATTR_MAX_LOCKED_TARGETS);
    let ship_limit = fit.value(Key::Ship, ATTR_MAX_LOCKED_TARGETS);
    let max_targets = if pilot_limit > 0.0 { ship_limit.min(pilot_limit) } else { ship_limit } as usize;
    if targets_of(cx.char_id).len() >= max_targets.max(1) {
        return Err(user_error(messages::MSG_CUSTOM_NOTIFY, vec![("notify".into(), Value::unicode(messages::target_limit(max_targets.max(1))))]));
    }
    let delay = lock_time_ms(fit.value(Key::Ship, ATTR_SCAN_RESOLUTION), attr(cx.world.sde(), t.type_id, ATTR_SIGNATURE_RADIUS, t.radius as f64));
    let attempt = NEXT_LOCK.fetch_add(1, Ordering::Relaxed);
    state().locking.insert((cx.char_id, target), attempt);
    debug!(user = c.login.user_id, "locking {target} in {delay}ms");
    let current = targets_of(cx.char_id);
    let world = cx.world.clone();
    world.schedule(cx.system_id, delay, move |s| {
        let still_wanted = {
            let mut st = state();
            st.locking.get(&(cx.char_id, target)) == Some(&attempt) && st.locking.remove(&(cx.char_id, target)).is_some()
        };
        let present = s.contains(ship_id) && s.contains(target);
        if still_wanted && present {
            state().targets.entry(cx.char_id).or_default().insert(target);
            cx.notify("OnTarget", vec![Value::str("add"), Value::Int(target)]);
        } else if still_wanted {
            cx.notify("OnTarget", vec![Value::str("lost"), Value::Int(target), Value::None]);
        }
    });
    Ok(Value::tuple([Value::Bool(true), Value::list(current.into_iter().map(Value::Int))]))
}

pub(super) fn drop_targets(c: &mut Client, targets: &[i64]) {
    let Some(char_id) = c.session.int("charid") else { return };
    let removed = {
        let mut st = state();
        targets.iter().copied().filter(|t| st.targets.get_mut(&char_id).is_some_and(|set| set.remove(t))).collect()
    };
    remove_target_projections(c, removed);
    for t in targets {
        c.notify("OnTarget", vec![Value::str("lost"), Value::Int(*t), Value::None]);
    }
}

/// Remove outgoing effects for locks removed by an explicit targeting operation.
pub(super) fn remove_target_projections(c: &Client, targets: Vec<i64>) {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return };
    if targets.is_empty() { return; }
    c.world.post(system, move |s| remove_target_projections_in_system(s, ship, &targets));
}

fn remove_target_projections_in_system(s: &mut crate::space::System, ship: i64, targets: &[i64]) {
    for &target in targets { s.remove_projections_between(ship, target); }
}

/// A destroyed ball is no longer anyone's target.
pub(super) fn forget_target(world: &World, target: i64) {
    let holders: Vec<i64> = {
        let mut st = state();
        st.hp.remove(&target);
        st.locking.retain(|(_, t), _| *t != target);
        st.targets.iter_mut().filter_map(|(ch, set)| set.remove(&target).then_some(*ch)).collect()
    };
    for ch in holders {
        world.notify_pilot(ch, "OnTarget", vec![Value::str("lost"), Value::Int(target), Value::str("Exploding")]);
    }
}

// ---------------------------------------------------------------- charges

/// The charge sublocation key `(shipID, flagID, typeID)`.
pub(crate) fn charge_key(ship: i64, flag: i32, type_id: i32) -> Value {
    Value::tuple([Value::Int(ship), Value::Int(flag as i64), Value::Int(type_id as i64)])
}

/// `OnModuleAttributeChanges` for a charge sublocation's quantity (clientDogmaLocation
/// fits/unfits the sublocation from it).


/// `{shipID: {flagID: row(locationID, flagID, typeID, quantity)}}` for the ship state.


pub(crate) async fn loaded(db: &(impl Executor + ?Sized), ship: i64, flag: i32) -> Result<Option<(i32, i32)>, CallError> {
    Ok(crate::dao::items::charges::loaded(db, ship, flag)
        .await
        .map_err(db_err)?
        .map(|r| (r.type_id, r.quantity)))
}

use super::super::items::deposit;

/// Modules of a weapon group, master first. The input must be the master.
pub(crate) async fn bank(db: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Vec<i64>, CallError> {
    let rows = crate::dao::items::charges::slaves(db, ship, master).await.map_err(db_err)?;
    Ok(std::iter::once(master).chain(rows.iter().map(|r| r.slave_id)).collect())
}

/// Resolve a bank member to its master, or return an ungrouped module unchanged.
pub(in crate::services) async fn master_for_member(ex: &(impl Executor + ?Sized), ship: i64, member: i64) -> Result<i64, CallError> {
    let rows = crate::dao::items::charges::banks(ex, ship).await.map_err(db_err)?;
    Ok(bank_master(&rows, member))
}

fn bank_master(rows: &[crate::dao::items::charges::Bank], member: i64) -> i64 {
    rows.iter().find(|row| row.slave_id == member).map_or(member, |row| row.master_id)
}

/// Empty a module's charges into `location`/`flag` (cargo by default).
pub(in crate::services) async fn unload<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, owner: i64, ship: i64, module: &Item, location: i64, flag: i32) -> Result<(), CallError> {
    let Some((type_id, qty)) = loaded(db, ship, module.flag_id).await? else { return Ok(()) };
    crate::dao::items::combat::delete_charge(db, ship, module.flag_id).await.map_err(db_err)?;
    outbox.notify_client("OnModuleAttributeChanges", vec![quantity_change(owner, ship, module.flag_id, type_id, 0, qty as i64)]);
    deposit(db, sde, outbox, owner, location, flag, type_id, qty, ship).await
}

// Unlike value_ids, scalar module IDs accept Bool through as_int; lists take precedence.
pub(super) fn module_ids(v: Option<&Value>) -> Vec<i64> {
    match v {
        Some(v) if v.items().is_some() => v.items().unwrap().iter().filter_map(|m| m.as_int()).collect(),
        Some(v) => v.as_int().into_iter().collect(),
        None => vec![],
    }
}

/// LoadAmmo(shipID, moduleID | [moduleIDs], [chargeItemIDs], chargeLocationID): fill each
/// module (and its group) with charges from the given stacks, swapping out other types.
/// In space the charges go in after the module's reload time (OnChargeBeingLoadedToModule
/// and nextActivationTime drive the client's reload animation); docked it is immediate.
pub(super) async fn load_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let modules = module_ids(call.args.get(1));
    let charge_ids = value_id_list(call.args.get(2));
    let charge_location = call.args.get(3).and_then(|v| v.as_int());
    load_ammo_core(c, ship, modules, charge_ids, charge_location).await
}

pub(in crate::services) async fn load_ammo_core(c: &mut Client, ship: i64, modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>) -> CallResult {
    load_ammo_impl(c, ship, modules, charge_ids, charge_location, None).await.map(|_| Value::None)
}

/// Generate a charge stack in the same transaction that reserves it for reload.
pub(in crate::services) async fn gm_load_ammo(c: &mut Client, ship: i64, modules: Vec<i64>, source: i64, type_id: i32, quantity: i32) -> Result<usize, CallError> {
    load_ammo_impl(c, ship, modules, Vec::new(), Some(source), Some((type_id, quantity))).await
}

async fn load_ammo_impl(c: &mut Client, ship: i64, modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>, generated: Option<(i32, i32)>) -> Result<usize, CallError> {
    let db = c.db.clone();
    let world = c.world.clone();
    let notify = c.notify_tx.clone();
    let sde = c.sde.clone();
    let owner = c.session.int("charid");
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let system = c.session.int("solarsystemid");
    reload(&db, &world, &notify, sde, ship, modules, charge_ids,
        charge_location, active_ship, station, owner, system, false, None, generated, Some(c)).await
}

/// A provisional reload marker blocks activation while automatic reservation awaits SQL.
/// The actual actor deadline replaces it on success; cancellation and refusals clear it.
struct AutoReloadMarker(i64);

impl Drop for AutoReloadMarker {
    fn drop(&mut self) {
        clear_reload_markers(&mut state().reloading, &[self.0], i64::MAX);
    }
}

/// Reload an empty ancillary magazine from compatible cargo after its cycle stops.
pub(in crate::services) async fn auto_reload(cx: &Ctx, module_id: i64) -> Result<(), CallError> {
    {
        let mut st = state();
        if st.active.contains_key(&module_id) || st.reloading.contains_key(&module_id) { return Ok(()); }
        st.reloading.insert(module_id, i64::MAX);
    }
    let _marker = AutoReloadMarker(module_id);
    let ms = Fit::load_in_space(&cx.db, &cx.world, cx.system_id, cx.char_id, cx.ship_id).await?
        .value(Key::Module(module_id), ATTR_RELOAD_TIME);
    let reload_ms = if ms > 0.0 { ms } else { DEFAULT_RELOAD_MS };
    reload(&cx.db, &cx.world, &cx.notify, cx.world.sde().clone(), cx.ship_id, vec![module_id],
        Vec::new(), Some(cx.ship_id), Some(cx.ship_id), None, Some(cx.char_id),
        Some(cx.system_id), true, Some(reload_ms), None, None).await.map(|_| ())
}

/// SDE dogma attribute names for module-compatible charge groups.
const CHARGE_GROUP_ATTRIBUTES: [&str; 5] = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"];

fn charge_matches(mut groups: impl Iterator<Item = f64>, group: Option<i32>, module_size: Option<f64>, charge_size: Option<f64>) -> bool {
    group.is_some_and(|group| groups.any(|value| value == f64::from(group)))
        && match module_size {
            Some(size) if size > 0.0 => charge_size == Some(size),
            _ => true,
        }
}

fn compatible_charge(sde: &Sde, module_type: i32, charge: &Item) -> bool {
    let Some(module) = sde.types.get(module_type) else { return false };
    let Some(charge_type) = sde.types.get(charge.type_id) else { return false };
    let groups = CHARGE_GROUP_ATTRIBUTES.iter().flat_map(|name| sde.dogma.attributes_by_name(name))
        .filter_map(|attr| module.attribute(attr.attribute_id));
    let size = sde.dogma.attributes_by_name("chargeSize").next().map(|attr| attr.attribute_id);
    charge.category_id == CATEGORY_CHARGE as i64 && charge_matches(groups, charge_type.group_id,
        size.and_then(|id| module.attribute(id)), size.and_then(|id| charge_type.attribute(id)))
}

async fn reload(db: &Db, world: &Arc<World>, notify: &NotifyTx, sde: Arc<Sde>, ship: i64,
    modules: Vec<i64>, charge_ids: Vec<i64>, charge_location: Option<i64>, active_ship: Option<i64>,
    station: Option<i64>, session_owner: Option<i64>, system: Option<i64>, automatic: bool,
    automatic_reload_ms: Option<f64>, generated: Option<(i32, i32)>, mut client: Option<&mut Client>) -> Result<usize, CallError> {
    let owner = session_owner.unwrap_or(0);
    let db = db.clone();
    let world = world.clone();
    let notify = notify.clone();
    let loads = db.transaction(
        |tx, outbox: &mut Outbox| {
            let modules = modules.clone();
            let mut charge_ids = charge_ids.clone();
            let sde = sde.clone();
            Box::pin(async move {
                let refuse = || notify_error(messages::INSUFFICIENT_AMMUNITION);
                if !crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)? { return Err(refuse()); }
                let owner = session_owner.ok_or_else(refuse)?;
                let hull = load_items(tx, &sde, ItemQuery::Id(ship)).await.map_err(db_err)?
                    .into_iter().next().ok_or_else(refuse)?;
                if hull.owner_id != owner || hull.category_id != crate::consts::CATEGORY_SHIP as i64
                    || (active_ship != Some(ship) && !(hull.singleton && station == Some(hull.location_id) && hull.flag_id == FLAG_HANGAR)) {
                    return Err(refuse());
                }
                let source = charge_location.ok_or_else(refuse)?;
                let source_flags: &[i32] = if station == Some(source) {
                    &[FLAG_HANGAR]
                } else {
                    let container = load_items(tx, &sde, ItemQuery::Id(source)).await.map_err(db_err)?
                        .into_iter().next().ok_or_else(refuse)?;
                    if container.owner_id != owner || !container.singleton {
                        return Err(refuse());
                    }
                    let is_ship = container.category_id == crate::consts::CATEGORY_SHIP as i64;
                    let in_hangar = station == Some(container.location_id) && container.flag_id == FLAG_HANGAR;
                    let is_active_ship = is_ship && source == ship && active_ship == Some(ship);
                    if !in_hangar && !is_active_ship
                        && (is_ship || container.location_id != ship
                            || !matches!(container.flag_id, FLAG_CARGO | FLAG_FLEET_HANGAR)) {
                        return Err(refuse());
                    }
                    if is_ship { &[FLAG_CARGO, FLAG_AMMO_HOLD, FLAG_FLEET_HANGAR] } else { &[FLAG_HANGAR] }
                };
                if let Some((type_id, quantity)) = generated {
                    let flag = if station == Some(source) { FLAG_HANGAR } else { FLAG_CARGO };
                    let id = crate::dao::items::transfers::insert(tx, type_id, owner, source, flag, quantity).await.map_err(db_err)?;
                    notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(0))]).await?;
                    charge_ids.push(id);
                }
                let mut all = Vec::new();
                for m in modules {
                    let master = if generated.is_some() { master_for_member(tx, ship, m).await? } else { m };
                    for id in bank(tx, ship, master).await? {
                        if !all.contains(&id) { all.push(id); }
                    }
                }
                let fitted = load_items(tx, &sde, ItemQuery::Ids(&all)).await.map_err(db_err)?;
                if all.iter().any(|id| !fitted.iter().any(|item| item.item_id == *id && item.owner_id == owner && item.location_id == ship && super::super::inventory::is_fitting_flag(item.flag_id))) {
                    return Err(refuse());
                }
                let mut target_flags: Vec<i32> = fitted.iter().map(|item| item.flag_id).collect();
                target_flags.sort_unstable();
                target_flags.dedup();
                let older = crate::dao::items::pending_reloads::for_flags(tx, ship, &target_flags).await.map_err(db_err)?;
                if automatic {
                    if !older.is_empty() { return Ok(Vec::new()); }
                    let Some(module) = fitted.first() else { return Ok(Vec::new()) };
                    if loaded(tx, ship, module.flag_id).await?.is_some_and(|(_, quantity)| quantity > 0) {
                        return Ok(Vec::new());
                    }
                }
                let older_ids: Vec<i64> = older.into_iter().map(|row| row.reload_id).collect();
                super::reload_recovery::return_pending(tx, &sde, outbox, &older_ids, true).await?;
                let stacks = if automatic {
                    let module = &fitted[0];
                    load_items(tx, &sde, ItemQuery::OwnerLocationFlag(owner, ship, FLAG_CARGO)).await.map_err(db_err)?
                        .into_iter().filter(|item| item.quantity > 0 && compatible_charge(&sde, module.type_id, item)).collect()
                } else {
                    load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?
                };
                if charge_ids.iter().any(|id| !stacks.iter().any(|item| item.item_id == *id && item.owner_id == owner && item.location_id == source && source_flags.contains(&item.flag_id) && item.quantity > 0)) {
                    return Err(refuse());
                }
                let selected: Vec<i64> = if automatic { stacks.iter().map(|item| item.item_id).collect() } else { charge_ids.clone() };
                let mut items: BTreeMap<i64, (Item, i32)> = stacks.into_iter().map(|item| (item.item_id, { let quantity = item.quantity; (item, quantity) })).collect();
                let mut inserts: BTreeMap<(i64, i32), i32> = BTreeMap::new();
                let mut unloads = BTreeMap::new();
                let mut loads = Vec::new();
                let mut flags = HashSet::new();
                for module_id in all {
                    let module = fitted.iter().find(|item| item.item_id == module_id).ok_or_else(refuse)?;
                    if !flags.insert(module.flag_id) { continue; }
                    let Some(charge) = selected.iter().find_map(|id| items.get(id).filter(|(_, q)| *q > 0).map(|(item, _)| item)) else { break };
                    if !compatible_charge(&sde, module.type_id, charge) { return Err(refuse()); }
                    let type_id = charge.type_id;
                    let module_type = sde.types.get(module.type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {}", module.type_id)))?;
                    let capacity = module_type.capacity.unwrap_or(0.0).max(module_type.attribute(ATTR_CAPACITY).unwrap_or(0.0));
                    let volume = sde.types.get(type_id).ok_or_else(|| CallError::Internal(anyhow::anyhow!("unknown type {type_id}")))?.volume.unwrap_or(0.01);
                    let max = (capacity / volume.max(1e-6) + 1e-6).floor() as i32;
                    if max <= 0 { continue; }
                    let have = match loaded(tx, ship, module.flag_id).await? {
                        Some((t, q)) if t == type_id => q,
                        Some((t, q)) => {
                            let unload_owner = session_owner.unwrap_or(module.owner_id);
                            unloads.insert(module.flag_id, (t, q, unload_owner));
                            if q > 0 {
                                let destination = load_items(tx, &sde, ItemQuery::LocationFlagTypeOwnerStack(ship, FLAG_CARGO, t, unload_owner)).await.map_err(db_err)?.into_iter().next();
                                if let Some(item) = destination {
                                    let entry = items.entry(item.item_id).or_insert_with(|| { let quantity = item.quantity; (item, quantity) });
                                    entry.1 = entry.1.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                                } else {
                                    let entry = inserts.entry((unload_owner, t)).or_default();
                                    *entry = entry.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                                }
                            }
                            0
                        }
                        None => 0,
                    };
                    if let Some(now) = reserve_load(&mut items, &selected, type_id, max, have) {
                        loads.push((module_id, module.flag_id, type_id, now, have));
                    }
                }
                let changes = items.iter().filter_map(|(&id, (item, quantity))| {
                    (*quantity != item.quantity).then(|| (id, QuantityChange::new(item, (*quantity > 0).then_some(*quantity))))
                }).collect();
                if !quantities::apply(tx, &changes).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_AMMUNITION)); }
                if !crate::dao::items::combat::delete_reserved(tx, ship, &unloads).await.map_err(db_err)? { return Err(notify_error(messages::INSUFFICIENT_AMMUNITION)); }
                for (&flag, &(type_id, quantity, unload_owner)) in &unloads {
                    outbox.notify_client("OnModuleAttributeChanges", vec![quantity_change(unload_owner, ship, flag, type_id, 0, quantity as i64)]);
                }
                for (id, (item, quantity)) in items {
                    if quantity == item.quantity { continue; }
                    if quantity == 0 { notify_item_removed(outbox, item); }
                    else { notify_item_change(tx, &sde, outbox, id, vec![(IX_STACK_SIZE, Value::Int(item.quantity as i64))]).await?; }
                }
                let ids = crate::dao::items::combat::insert_cargo(tx, ship, FLAG_CARGO, &inserts).await.map_err(db_err)?;
                for id in ids {
                    notify_item_change(tx, &sde, outbox, id, vec![(IX_LOCATION_ID, Value::Int(ship)), (IX_FLAG, Value::Int(FLAG_CARGO as i64))]).await?;
                }
                let mut pending = Vec::with_capacity(loads.len());
                for load in loads {
                    let (module, flag, type_id, now, have) = load;
                    let reload_id = crate::dao::items::pending_reloads::insert(tx, owner, ship, module, flag, type_id, now - have).await.map_err(db_err)?;
                    pending.push(PendingLoad { reload_id, load });
                }
                Ok(pending)
            })
        },
        |value, outbox| {
            let world = &world;
            let notify = &notify;
            let client = &mut client;
            async move {
                for entry in outbox.into_entries() {
                    match entry {
                        crate::db::outbox::Effect::Notification(n) => {
                            if let Some(client) = client.as_deref_mut() { n.dispatch(world, Some(client))?; }
                            else { n.dispatch_sender(world, notify)?; }
                        }
                        crate::db::outbox::Effect::Event(event) => crate::lua::on_event(event),
                        crate::db::outbox::Effect::Operation(never) => match never {},
                    }
                }
                Ok(value)
            }
        },
    ).await?;
    let count = loads.len();
    if loads.is_empty() { return Ok(0); }
    let Some(system) = system else {
        apply_loads(&db, &world, &notify, ship, loads).await?;
        return Ok(count);
    };
    let groups = if let Some(ms) = automatic_reload_ms {
        let type_id = loads[0].load.2;
        BTreeMap::from([((filetime_ticks_from_ms(ms), type_id), loads.into_iter().map(|load| (load, ms)).collect())])
    } else {
        let fit = Fit::load_in_space(&db, &world, system, owner, ship).await?;
        if generated.is_some() {
            group_loads(loads, |module| fit.value(Key::Module(module), ATTR_RELOAD_TIME))
        } else {
            let first = loads[0].load;
            let ms = match fit.value(Key::Module(first.0), ATTR_RELOAD_TIME) {
                ms if ms > 0.0 => ms,
                _ => DEFAULT_RELOAD_MS,
            };
            BTreeMap::from([((filetime_ticks_from_ms(ms), first.2), loads.into_iter().map(|load| (load, ms)).collect())])
        }
    };
    let start = filetime_now();
    for ((ticks, type_id), loads) in groups {
        let until = start + ticks;
        let reload_ms = loads[0].1;
        let loads: Vec<_> = loads.into_iter().map(|(load, _)| load).collect();
        let ids: Vec<i64> = loads.iter().map(|l| l.load.0).collect();
        {
            let mut st = state();
            for id in &ids { st.reloading.insert(*id, until); }
        }
        // (moduleIDs, chargeTypeID, reloadTime ms): the HUD's reload ramp.
        let ramp = vec![Value::list(ids.iter().map(|i| Value::Int(*i))), Value::Int(type_id as i64), Value::Int(reload_ms as i64)];
        let timers = ids.iter().map(|i| attribute_change(owner, Value::Int(*i), ATTR_NEXT_ACTIVATION_TIME, Value::Int(until), Value::Int(start))).collect();
        if let Some(client) = client.as_deref_mut() {
            client.notify("OnChargeBeingLoadedToModule", ramp);
            client.notify("OnModuleAttributeChanges", vec![Value::List(timers)]);
        } else {
            let _ = notify.send(("OnChargeBeingLoadedToModule".into(), ramp));
            let _ = notify.send(("OnModuleAttributeChanges".into(), vec![Value::List(timers)]));
        }
        let timer_world = world.clone();
        let db = db.clone();
        let notify = notify.clone();
        let deadline = crate::client::unix_ms_from_filetime_ceil(until);
        world.post(system, move |s| {
            s.schedule_at(deadline, move |s| {
                s.defer(async move {
                    if let Err(e) = apply_loads(&db, &timer_world, &notify, ship, loads).await {
                        warn!("reload of {ids:?} failed: {}", match e {
                            CallError::Internal(e) => format!("{e:#}"),
                            CallError::User { msg, .. } => msg,
                        });
                    }
                    let mut st = state();
                    clear_reload_markers(&mut st.reloading, &ids, until);
                });
            });
        });
    }
    Ok(count)
}

fn reserve_load(items: &mut BTreeMap<i64, (Item, i32)>, selected: &[i64], type_id: i32, max: i32, have: i32) -> Option<i32> {
    let need = reserve_quantities(items, selected, type_id, max.saturating_sub(have));
    let now = max - need;
    (now > have).then_some(now)
}

pub(super) fn reserve_quantities(items: &mut BTreeMap<i64, (Item, i32)>, selected: &[i64], type_id: i32, mut need: i32) -> i32 {
    for id in selected {
        let (item, remaining) = items.get_mut(id).unwrap();
        if item.type_id != type_id || need <= 0 { continue; }
        let take = need.min((*remaining).max(0));
        *remaining -= take;
        need -= take;
    }
    need
}
pub(super) type ReservedLoad = (i64, i32, i32, i32, i32);

#[derive(Clone)]
struct PendingLoad {
    reload_id: i64,
    load: ReservedLoad,
}

fn group_loads(loads: Vec<PendingLoad>, duration: impl Fn(i64) -> f64) -> BTreeMap<(i64, i32), Vec<(PendingLoad, f64)>> {
    let mut groups = BTreeMap::new();
    for load in loads {
        let ms = match duration(load.load.0) { ms if ms > 0.0 => ms, _ => DEFAULT_RELOAD_MS };
        groups.entry((filetime_ticks_from_ms(ms), load.load.2)).or_insert_with(Vec::new).push((load, ms));
    }
    groups
}

pub(super) fn clear_reload_markers(reloading: &mut HashMap<i64, i64>, ids: &[i64], until: i64) {
    for id in ids {
        if reloading.get(id) == Some(&until) { reloading.remove(id); }
    }
}

/// Put loaded charges in their modules and tell the client.
async fn apply_loads(db: &Db, world: &World, notify: &NotifyTx, ship: i64, mut loads: Vec<PendingLoad>) -> Result<(), CallError> {
    loads.sort_by_key(|load| load.reload_id);
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let loads = loads.clone();
            let notify = notify.clone();
            Box::pin(async move {
                let exists = crate::dao::items::ship::lock_reload_ship(tx, ship).await.map_err(db_err)?;
                let mut entries: Vec<(i64, ReservedLoad)> = Vec::new();
                for pending in loads {
                    let Some(row) = crate::dao::items::pending_reloads::take(tx, pending.reload_id).await.map_err(db_err)? else { continue };
                    if !exists { continue; }
                    let (_, _, _, now, have) = pending.load;
                    entries.push((row.owner_id, (row.module_id, row.flag_id, row.type_id, now, have)));
                }
                if !entries.is_empty() {
                    entries.sort_by_key(|(_, load)| load.1);
                    let loads: Vec<ReservedLoad> = entries.iter().map(|(_, load)| *load).collect();
                    crate::dao::items::combat::apply_loads(tx, ship, &loads).await.map_err(db_err)?;
                    for (owner, (module, flag, type_id, now, have)) in entries {
                        outbox.notify_sender(notify.clone(), "OnModuleAttributeChanges", vec![quantity_change(owner, ship, flag, type_id, now as i64, have as i64)]);
                        debug!("loaded {now} x {type_id} into module {module}");
                    }
                }
                Ok(())
            })
        },
        |value, outbox| async move {
            for entry in outbox.into_entries() {
                match entry {
                    crate::db::outbox::Effect::Notification(n) => n.dispatch_sender(world, notify)?,
                    crate::db::outbox::Effect::Event(event) => crate::lua::on_event(event),
                    crate::db::outbox::Effect::Operation(never) => match never {},
                }
            }
            Ok(value)
        },
    ).await
}

/// UnloadAmmo(shipID, moduleID | [moduleIDs], destination[, quantity]); destination is a
/// container ID or (containerID, ownerID, flag).
pub(super) async fn unload_ammo(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ship = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let modules = module_ids(call.args.get(1));
    let (location, flag) = match call.args.get(2) {
        Some(v) if v.items().is_some() => {
            let t = v.items().unwrap();
            (t.first().and_then(|x| x.as_int()).unwrap_or(ship), t.get(2).and_then(|x| x.as_int()).map(|f| f as i32))
        }
        Some(v) => (v.as_int().unwrap_or(ship), None),
        None => (ship, None),
    };
    let flag = flag.unwrap_or(if location == ship { FLAG_CARGO } else { FLAG_HANGAR });
    unload_ammo_core(c, ship, modules, location, flag).await
}

pub(in crate::services) async fn unload_ammo_core(c: &mut Client, ship: i64, modules: Vec<i64>, location: i64, flag: i32) -> CallResult {
    let owner = c.session.int("charid");
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        |tx, outbox: &mut Outbox| {
            let modules = modules.clone();
            let sde = sde.clone();
            Box::pin(async move {
                for m in modules {
                    for id in bank(tx, ship, m).await? {
                        if let Some(module) = load_items(tx, &sde, ItemQuery::IdAt(id, ship)).await.map_err(db_err)?.into_iter().next() {
                            unload(tx, &sde, outbox, owner.unwrap_or(module.owner_id), ship, &module, location, flag).await?;
                        }
                    }
                }
                Ok(Value::None)
            })
        },
        |value, outbox| async move { dispatch_client(value, outbox, c) },
    ).await
}


#[cfg(test)] mod tests;
