//! Combat targeting and ammunition loading, mirroring eve/client/script/dogma/clientDogmaLocation.py.
//! Uses game.items, game.module_charges and game.pending_reloads for inventory and reservations.
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
    let fit = Fit::load(&cx.db, cx.world.sde().clone(), cx.char_id, cx.ship_id).await?;
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
    {
        let mut st = state();
        if let Some(set) = st.targets.get_mut(&char_id) {
            for t in targets {
                set.remove(t);
            }
        }
    }
    for t in targets {
        c.notify("OnTarget", vec![Value::str("lost"), Value::Int(*t), Value::None]);
    }
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

/// Modules of a weapon group, master first.
pub(crate) async fn bank(db: &(impl Executor + ?Sized), ship: i64, master: i64) -> Result<Vec<i64>, CallError> {
    let rows = crate::dao::items::charges::slaves(db, ship, master).await.map_err(db_err)?;
    Ok(std::iter::once(master).chain(rows.iter().map(|r| r.slave_id)).collect())
}

/// Empty a module's charges into `location`/`flag` (cargo by default).
pub(super) async fn unload<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, owner: i64, ship: i64, module: &Item, location: i64, flag: i32) -> Result<(), CallError> {
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
    let active_ship = c.session.int("shipid");
    let station = c.session.int("stationid");
    let session_owner = c.session.int("charid");
    let owner = session_owner.unwrap_or(0);
    let db = c.db.clone();
    let sde = c.sde.clone();
    let client = &mut *c;
    let loads = db.transaction(
        |tx, outbox: &mut Outbox| {
            let modules = modules.clone();
            let charge_ids = charge_ids.clone();
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
                let mut all = Vec::new();
                for m in modules {
                    for id in bank(tx, ship, m).await? {
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
                let older_ids: Vec<i64> = older.into_iter().map(|row| row.reload_id).collect();
                super::reload_recovery::return_pending(tx, &sde, outbox, &older_ids, true).await?;
                let stacks = load_items(tx, &sde, ItemQuery::Ids(&charge_ids)).await.map_err(db_err)?;
                if charge_ids.iter().any(|id| !stacks.iter().any(|item| item.item_id == *id && item.owner_id == owner && item.location_id == source && source_flags.contains(&item.flag_id) && item.quantity > 0)) {
                    return Err(refuse());
                }
                let selected: Vec<i64> = stacks.iter().map(|item| item.item_id).collect();
                let mut items: BTreeMap<i64, (Item, i32)> = stacks.into_iter().map(|item| (item.item_id, { let quantity = item.quantity; (item, quantity) })).collect();
                let mut inserts: BTreeMap<(i64, i32), i32> = BTreeMap::new();
                let mut unloads = BTreeMap::new();
                let mut loads = Vec::new();
                let mut flags = HashSet::new();
                for module_id in all {
                    let module = fitted.iter().find(|item| item.item_id == module_id).ok_or_else(refuse)?;
                    if !flags.insert(module.flag_id) { continue; }
                    let Some(type_id) = selected.iter().find_map(|id| items.get(id).filter(|(_, q)| *q > 0).map(|(item, _)| item.type_id)) else { break };
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
                            let destination = load_items(tx, &sde, ItemQuery::LocationFlagTypeOwnerStack(ship, FLAG_CARGO, t, unload_owner)).await.map_err(db_err)?.into_iter().next();
                            if let Some(item) = destination {
                                let entry = items.entry(item.item_id).or_insert_with(|| { let quantity = item.quantity; (item, quantity) });
                                entry.1 = entry.1.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                            } else {
                                let entry = inserts.entry((unload_owner, t)).or_default();
                                *entry = entry.checked_add(q).ok_or_else(|| notify_error(messages::INSUFFICIENT_AMMUNITION))?;
                            }
                            0
                        }
                        None => 0,
                    };
                    let need = reserve_quantities(&mut items, &selected, type_id, max.saturating_sub(have));
                    let now = max - need;
                    if now > have { loads.push((module_id, module.flag_id, type_id, now, have)); }
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
        |value, outbox| async move { dispatch_client(value, outbox, client) },
    ).await?;
    let Some(&(first, _, type_id, _, _)) = loads.first().map(|pending| &pending.load) else { return Ok(Value::None) };
    let notify = c.notify_tx.clone();
    if c.session.int("solarsystemid").is_none() {
        apply_loads(&db, &c.world, &notify, ship, loads).await?;
        return Ok(Value::None);
    }
    let reload_ms = match Fit::load(&c.db, c.sde.clone(), owner, ship).await?.value(Key::Module(first), ATTR_RELOAD_TIME) {
        ms if ms > 0.0 => ms,
        _ => DEFAULT_RELOAD_MS,
    };
    let start = filetime_now();
    let until = start + filetime_ticks_from_ms(reload_ms);
    let ids: Vec<i64> = loads.iter().map(|l| l.load.0).collect();
    {
        let mut st = state();
        for id in &ids {
            st.reloading.insert(*id, until);
        }
    }
    // (moduleIDs, chargeTypeID, reloadTime ms): the HUD's reload ramp and "done in N s" message.
    c.notify("OnChargeBeingLoadedToModule", vec![Value::list(ids.iter().map(|i| Value::Int(*i))), Value::Int(type_id as i64), Value::Int(reload_ms as i64)]);
    let timers = ids.iter().map(|i| attribute_change(owner, Value::Int(*i), ATTR_NEXT_ACTIVATION_TIME, Value::Int(until), Value::Int(start))).collect();
    c.notify("OnModuleAttributeChanges", vec![Value::List(timers)]);
    info!(user = c.login.user_id, "reloading {} module(s) with {type_id} in {reload_ms:.0} ms", ids.len());
    let system = c.session.int("solarsystemid").unwrap();
    let world = c.world.clone();
    c.world.schedule(system, seconds_to_ms(ms_to_seconds(reload_ms)), move |s| {
        s.defer(async move {
            if let Err(e) = apply_loads(&db, &world, &notify, ship, loads).await {
                warn!("reload of {ids:?} failed: {}", match e {
                    CallError::Internal(e) => format!("{e:#}"),
                    CallError::User { msg, .. } => msg,
                });
            }
            let mut st = state();
            clear_reload_markers(&mut st.reloading, &ids, until);
        });
    });
    Ok(Value::None)
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


