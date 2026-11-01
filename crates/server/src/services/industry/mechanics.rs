//! Industry job calculations and lifecycle for the client industry package.
//! Uses blueprint SDE data and game jobs, items and wallets; corporation jobs are unsupported.
//! Job durations include the configured industry time multiplier; NPC fees use the published
//! facility policy and submitted cost and facility-tax quotes are checked before settlement.

use super::*;
use crate::sde::{Sde, industry::Activity};


/// industryCommon.GetBlueprintPrice: the adjusted value of what the blueprint's manufacturing
/// takes, at base quantities (the estimated item value of one run).
async fn blueprint_price(db: &(impl Executor + ?Sized), sde: &Sde, bp_type: i32) -> Result<f64, CallError> {
    let Some(m) = sde.industry.activity(bp_type, MANUFACTURING) else { return Ok(0.0) };
    let ids: Vec<i32> = m.materials.iter().map(|(t, _)| *t).collect();
    let price = industry_dao::prices(db, &ids).await.map_err(db_err)?;
    Ok(m.materials.iter().map(|(t, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum())
}

async fn blueprint(db: &(impl Executor + ?Sized), sde: &Sde, item_id: i64) -> Result<Blueprint, CallError> {
    let mut bp = industry_dao::blueprint(db, item_id).await.map_err(db_err)?
        .ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
    bp.station = bp.station.filter(|id| sde.map.station(*id).is_some());
    Ok(bp)
}

async fn set_blueprint(db: &(impl Executor + ?Sized), item_id: i64, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    industry_dao::set_blueprint(db, item_id, me, te, runs).await.map_err(db_err)
}

/// Decryptor bonuses from explicitly present type attributes.
fn decryptor(sde: &Sde, type_id: i32) -> (f64, i32, i32, i32) {
    let attr = |a, d| sde.types.attribute(type_id, a).unwrap_or(d);
    (attr(ATTR_INVENTION_PROBABILITY_MULTIPLIER, 1.0),
     attr(ATTR_INVENTION_ME_MODIFIER, 0.0) as i32,
     attr(ATTR_INVENTION_TE_MODIFIER, 0.0) as i32,
     attr(ATTR_INVENTION_MAX_RUN_MODIFIER, 0.0) as i32)
}

/// What a job will take, how long and what it costs, as the industry package computes it.
struct Plan {
    materials: Vec<(i32, i64)>,
    seconds: i64,
    fees: Fees,
    probability: f64,
    product: Option<i32>,
}

struct Fees {
    indexed_cost: f64,
    facility_tax: f64,
    scc: f64,
    security_tax: f64,
}

impl Fees {
    fn total(&self) -> f64 {
        self.indexed_cost + self.facility_tax + self.scc + self.security_tax
    }
}

/// The client rounds the base before each fee, but applies the index before rounding cost.
fn fees(job_cost: f64, activity: i32, security: f64, index: f64, tax: f64, scc_rate: f64, highsec_rate: f64) -> Fees {
    let base = job_cost.round();
    let scc_rate = if matches!(activity, RESEARCH_TIME | RESEARCH_MATERIAL) { scc_rate * 0.5 } else { scc_rate };
    Fees {
        indexed_cost: (job_cost * index).round(),
        facility_tax: (base * tax).round(),
        scc: (base * scc_rate).round(),
        security_tax: if security >= 0.45 { (base * highsec_rate).round() } else { 0.0 },
    }
}

fn validate_quote(cost: Option<&Value>, tax: Option<&Value>, fees: &Fees) -> Result<(), CallError> {
    for (value, expected) in [(cost, fees.indexed_cost), (tax, fees.facility_tax)] {
        if let Some(value) = value {
            if !value.as_float().is_some_and(|n| n.is_finite() && n == expected) {
                return Err(notify_error(messages::industry::QUOTE_CHANGED));
            }
        }
    }
    Ok(())
}

fn job_seconds(seconds: f64, multiplier: f64, runs: i32, max_run_secs: f64) -> Result<i64, CallError> {
    let seconds = seconds * multiplier;
    let rounded = seconds.round();
    if !seconds.is_finite()
        || rounded < i64::MIN as f64
        || rounded >= i64::MAX as f64
        || seconds - seconds / runs as f64 > max_run_secs
    {
        return Err(notify_error(messages::INDUSTRY_PROJECT_TOO_LONG));
    }
    Ok(rounded as i64)
}

fn job_deadline(now: i64, seconds: i64) -> Result<i64, CallError> {
    seconds.checked_mul(FILETIME_SECOND)
        .and_then(|duration| now.checked_add(duration))
        .ok_or_else(|| notify_error(messages::INDUSTRY_PROJECT_TOO_LONG))
}

#[allow(clippy::too_many_arguments)]
async fn plan(
    db: &(impl Executor + ?Sized),
    sde: &Sde,
    fit: &Fit,
    skills: &HashMap<i32, i32>,
    bp: &Blueprint,
    act: &Activity,
    activity_id: i32,
    runs: i32,
    licensed_runs: i32,
    requested_product: Option<i32>,
    optional: Option<i32>,
    facility: &Facility,
) -> Result<Plan, CallError> {
    let policy = &crate::config::game().industry;
    let level = |s: i32| skills.get(&s).copied().unwrap_or(0) as f64;
    // Materials: ME applies to manufacturing; copying multiplies by the licensed runs.
    let material_runs = if activity_id == COPYING { runs as f64 * licensed_runs as f64 } else { runs as f64 };
    let me_factor = if activity_id == MANUFACTURING { 1.0 - bp.me as f64 / 100.0 } else { 1.0 };
    let mut materials: Vec<(i32, i64)> = act
        .materials
        .iter()
        .map(|(t, q)| {
            let quantity = ((*q as f64 * me_factor * material_runs) * 100.0).round() / 100.0;
            (*t, (quantity.ceil() as i64).max(runs as i64))
        })
        .collect();
    let mut probability = 1.0;
    if activity_id == INVENTION {
        let base = act.products.iter().map(|p| p.2).sum::<f64>() / act.products.len().max(1) as f64;
        let skill_bonus = 1.0 + act.skills.iter().map(|(s, _)| level(*s) * policy.invention_skill_probability).sum::<f64>();
        let decryptor_multiplier = match optional {
            Some(d) => decryptor(sde, d).0,
            None => 1.0,
        };
        probability = (base * skill_bonus * decryptor_multiplier).clamp(0.0, 1.0);
        if let Some(d) = optional {
            materials.push((d, runs as i64));
        }
    }

    // Time: the activity's own formula, then TE and the character's industry attributes.
    let char_attr = |a: i32| {
        let v = fit.value(Key::Char, a);
        if v == 0.0 {
            1.0
        } else {
            v
        }
    };
    let research = |current: i32, step: i32, max: i32| {
        let levels: Vec<f64> = (0..(max / step) as usize).map(|i| act.time * RESEARCH_TIMES[i.min(RESEARCH_TIMES.len() - 1)]).collect();
        let current = (current / step) as usize;
        (0..runs as usize).map(|i| levels[(current + i).min(levels.len() - 1)]).sum::<f64>()
    };
    let base_seconds = match activity_id {
        RESEARCH_TIME => research(bp.te, 2, MAX_TIME_EFFICIENCY),
        RESEARCH_MATERIAL => research(bp.me, 1, MAX_MATERIAL_EFFICIENCY),
        COPYING => act.time * licensed_runs as f64 * runs as f64,
        _ => act.time * runs as f64,
    };
    let mut seconds = base_seconds;
    match activity_id {
        MANUFACTURING => {
            seconds *= 1.0 - bp.te as f64 / 100.0;
            seconds *= char_attr(ATTR_MANUFACTURE_TIME_MULTIPLIER);
            for (skill, _) in &act.skills {
                let per_level = sde.types.attribute(*skill, ATTR_MANUFACTURE_TIME_PER_LEVEL);
                if let Some(p) = per_level {
                    seconds *= 1.0 + p * level(*skill) / 100.0;
                }
            }
        }
        RESEARCH_TIME => seconds *= char_attr(ATTR_MANUFACTURING_TIME_RESEARCH_SPEED),
        RESEARCH_MATERIAL => seconds *= char_attr(ATTR_MINERAL_NEED_RESEARCH_SPEED),
        COPYING => seconds *= char_attr(ATTR_COPY_SPEED_PERCENT),
        INVENTION => seconds *= char_attr(ATTR_INVENTION_RESEARCH_SPEED),
        _ => {}
    }
    let seconds = job_seconds(seconds, policy.job_time_multiplier, runs, policy.max_run_secs)?;

    // Cost: the estimated item value, scaled per activity.
    let product = match activity_id {
        MANUFACTURING | INVENTION => {
            let products: Vec<i32> = act.products.iter().map(|p| p.0).collect();
            requested_product.filter(|p| products.contains(p)).or(products.first().copied())
        }
        _ => None,
    };
    let job_cost = match activity_id {
        MANUFACTURING => blueprint_price(db, sde, bp.type_id).await? * runs as f64,
        COPYING => blueprint_price(db, sde, bp.type_id).await? * policy.job_cost_rate * licensed_runs as f64 * runs as f64,
        RESEARCH_TIME | RESEARCH_MATERIAL if act.time > 0.0 => blueprint_price(db, sde, bp.type_id).await? * policy.job_cost_rate * base_seconds / act.time,
        INVENTION => match product {
            Some(p) => blueprint_price(db, sde, p).await? * policy.job_cost_rate * runs as f64,
            None => 0.0,
        },
        _ => 0.0,
    };
    let fees = fees(job_cost, activity_id, facility.security, facility.system_index(), facility.tax(),
        policy.scc_surcharge_rate, policy.highsec_security_tax);
    Ok(Plan { materials, seconds, fees, probability, product })
}

/// Active jobs per activity of an installer (GetJobCounts).
pub(super) async fn job_counts(db: &(impl Executor + ?Sized), installer: i64) -> Result<HashMap<i32, i64>, CallError> {
    industry_dao::job_counts(db, installer, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)
}

/// `InstallJob(job.dump())` -> jobID.
pub(super) async fn install(c: &Client, request: &Value) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let int = |k: &str| request.get(k).filter(|v| !v.is_none()).and_then(|v| v.as_int());
    let activity_id = int("activityID").unwrap_or(0) as i32;
    let max_runs = crate::config::game().industry.max_runs.min(i32::MAX as i64) as i32;
    let runs = int("runs").unwrap_or(1).clamp(1, max_runs as i64) as i32;
    let licensed_runs = int("licensedRuns").unwrap_or(1).max(1) as i32;
    let account_owner = request.get("account").and_then(|v| v.items()).and_then(|a| a.first()).and_then(|v| v.as_int()).unwrap_or(me);
    if account_owner != me {
        return Err(notify_error(messages::CORPORATION_INDUSTRY_UNSUPPORTED));
    }
    let requested: HashMap<i32, i64> = match request.get("materials") {
        Some(Value::Dict(pairs)) => pairs.iter().filter_map(|(k, v)| Some((k.as_int()? as i32, v.as_int().unwrap_or(0)))).collect(),
        _ => HashMap::new(),
    };
    let quoted_cost = request.get("cost").cloned();
    let quoted_tax = request.get("tax").cloned();
    let _trading = lock().await;
    let blueprint_id = int("blueprintID").unwrap_or(0);
    let facility_id = int("facilityID").unwrap_or(0);
    let product_request = int("productTypeID").map(|p| p as i32);
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let fit = Arc::new(Fit::load(db, c.sde.clone(), me, ship).await?);
    let sde = Arc::clone(&c.sde);
    let now = filetime_now();
    let j = db.transaction(
        |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let fit = Arc::clone(&fit);
            let sde = Arc::clone(&sde);
            let quoted_cost = quoted_cost.clone();
            let quoted_tax = quoted_tax.clone();
            Box::pin(async move {
                let mut bp = blueprint(tx, &sde, blueprint_id).await?;
                if bp.owner != me {
                    return Err(user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]));
                }
                let busy = industry_dao::blueprint_busy(tx, bp.item_id, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)?;
                if busy {
                    return Err(notify_error(messages::BLUEPRINT_IN_USE));
                }
                let fac = facility(&sde, facility_id)?;
                if !fac.activities().contains(&activity_id) {
                    return Err(notify_error(messages::INDUSTRY_ACTIVITY_UNAVAILABLE));
                }
                if bp.station != Some(fac.id) {
                    return Err(notify_error(messages::BLUEPRINT_MUST_BE_AT_FACILITY));
                }
                let Some(act) = sde.industry.activity(bp.type_id, activity_id) else {
                    return Err(notify_error(messages::BLUEPRINT_ACTIVITY_INVALID));
                };
                // Originals research and copy; only copies are invented from.
                let needs_original = matches!(activity_id, RESEARCH_TIME | RESEARCH_MATERIAL | COPYING);
                if needs_original && bp.copy || activity_id == INVENTION && !bp.copy {
                    return Err(notify_error(messages::BLUEPRINT_ACTIVITY_INVALID));
                }
                let max_runs = match activity_id {
                    RESEARCH_TIME => (MAX_TIME_EFFICIENCY - bp.te) / 2,
                    RESEARCH_MATERIAL => MAX_MATERIAL_EFFICIENCY - bp.me,
                    MANUFACTURING | INVENTION if bp.copy => bp.runs,
                    _ => max_runs,
                };
                if runs > max_runs || (activity_id == COPYING && licensed_runs > act.max_production_limit) {
                    return Err(notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED));
                }
                let skills: HashMap<i32, i32> = crate::dao::skills::levels::levels(tx, me).await.map_err(db_err)?.into_iter().map(|r| (r.type_id, r.level)).collect();
                if let Some((skill, level)) = act.skills.iter().find(|(s, l)| skills.get(s).copied().unwrap_or(0) < *l) {
                    return Err(notify_error(&messages::required_skill_message(skill, level)));
                }
                // Remote jobs need Supply Chain Management / Scientific Networking.
                let reach_skill = if activity_id == MANUFACTURING { SKILL_SUPPLY_CHAIN_MANAGEMENT } else { SKILL_SCIENTIFIC_NETWORKING };
                let reach = DISTANCE_PER_LEVEL[skills.get(&reach_skill).copied().unwrap_or(0).clamp(0, 5) as usize];
                if jumps(&sde.map, here, fac.system, reach).is_none() {
                    return Err(notify_error(messages::FACILITY_OUTSIDE_REMOTE_INDUSTRY_RANGE));
                }
                let slots = if activity_id == MANUFACTURING {
                    fit.value(Key::Char, ATTR_MANUFACTURE_SLOT_LIMIT)
                } else {
                    fit.value(Key::Char, ATTR_MAX_LABORATORY_SLOTS)
                }
                .max(1.0) as i64;
                let counts = job_counts(tx, me).await?;
                let used: i64 = if activity_id == MANUFACTURING {
                    counts.get(&MANUFACTURING).copied().unwrap_or(0)
                } else {
                    [RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION].iter().map(|a| counts.get(a).copied().unwrap_or(0)).sum()
                };
                if used >= slots {
                    return Err(notify_error(&messages::industry_slots_full_message(slots)));
                }
                let optional = if activity_id == INVENTION {
                    let mut decryptor = None;
                    for t in requested.keys() {
                        let group = sde.types.get(*t).and_then(|r| r.group_id);
                        if group == Some(GROUP_DECRYPTORS) {
                            decryptor = Some(*t);
                        }
                    }
                    decryptor
                } else {
                    None
                };
                let p = plan(tx, &sde, &fit, &skills, &bp, act, activity_id, runs, licensed_runs, product_request, optional, &fac).await?;
                validate_quote(quoted_cost.as_ref(), quoted_tax.as_ref(), &p.fees)?;
                let end = job_deadline(now, p.seconds)?;

                let have = available(tx, me, fac.id).await?;
                if let Some((t, q)) = p.materials.iter().find(|(t, q)| have.get(t).copied().unwrap_or(0) < *q) {
                    return Err(notify_error(&messages::required_materials_message(q, t)));
                }
                debit(tx, outbox, me, p.fees.total()).await?;
                for (type_id, quantity) in &p.materials {
                    let mut left = *quantity;
                    let stacks = load_items(tx, &sde, ItemQuery::OwnerLocationFlagTypeStackAscending(me, fac.id, FLAG_HANGAR, *type_id)).await.map_err(db_err)?;
                    for s in stacks {
                        if left == 0 {
                            break;
                        }
                        let n = (s.quantity as i64).min(left);
                        take_items(tx, outbox, s, n as i32).await?;
                        left -= n;
                    }
                }
                // A blueprint in a stack is split off as its own item for the job.
                if !bp.singleton {
                    let location = industry_dao::item_location(tx, bp.item_id).await.map_err(db_err)?;
                    if bp.quantity > 1 {
                        let stack = load_items(tx, &sde, ItemQuery::Id(bp.item_id)).await.map_err(db_err)?.pop().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
                        take_items(tx, outbox, stack, 1).await?;
                        bp.item_id = crate::dao::items::creation::rookie_ship(tx, bp.type_id, me, location, FLAG_HANGAR).await.map_err(db_err)?;
                        show_items(tx, &sde, outbox, me, location, &[bp.item_id], LOCATION_JUNKYARD).await?;
                    } else {
                        industry_dao::make_singleton(tx, bp.item_id).await.map_err(db_err)?;
                    }
                }
                let mut j = Job {
                    id: 0,
                    installer: me,
                    owner: me,
                    activity: activity_id,
                    blueprint_id: bp.item_id,
                    blueprint_type: bp.type_id,
                    blueprint_copy: bp.copy,
                    facility: fac.id,
                    system: fac.system,
                    runs,
                    licensed_runs,
                    successful_runs: None,
                    product_type: p.product,
                    optional_type: optional,
                    cost: p.fees.total(),
                    probability: p.probability,
                    time: p.seconds,
                    start: now,
                    end,
                    completed: None,
                    completed_by: None,
                    status: INDUSTRY_JOB_STATUS_INSTALLED,
                };
                j.id = industry_dao::insert_job(tx, &j).await.map_err(db_err)?;
                tell_job(outbox, &j);
                Ok(j)
            })
        },
        |j, outbox| async move { dispatch_world(j, outbox, world) },
    ).await?;
    info!("industry: {me} installed job {} (activity {activity_id}, {runs} runs of {}) at {} for {} ISK, {}s", j.id, j.blueprint_type, j.facility, j.cost, j.time);
    Ok(Value::Int(j.id))
}

/// Remove a used-up blueprint copy.
async fn destroy_blueprint<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, item_id: i64) -> Result<(), CallError> {
    if let Some(item) = load_items(db, sde, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop() {
        let owner = item.owner_id;
        industry_dao::delete_blueprint_item(db, item_id).await.map_err(db_err)?;
        let old = item.location_id;
        let gone = Item { location_id: LOCATION_JUNKYARD, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    }
    Ok(())
}

/// New blueprint copies in `owner`'s hangar at `station`.
#[allow(clippy::too_many_arguments)]
async fn make_copies<X: Send>(db: &(impl Executor + ?Sized), sde: &crate::sde::Sde, outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, count: i32, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    let mut ids = vec![];
    for _ in 0..count {
        let id = industry_dao::insert_copy(db, type_id, owner, station, FLAG_HANGAR, BLUEPRINT_COPY_QUANTITY).await.map_err(db_err)?;
        set_blueprint(db, id, me, te, runs).await?;
        ids.push(id);
    }
    show_items(db, sde, outbox, owner, station, &ids, LOCATION_JUNKYARD).await
}

/// Deliver a finished job's output to the owner's hangar at the facility.
async fn deliver_job<X: Send>(db: &(impl Executor + ?Sized), sde: &Sde, outbox: &mut Outbox<X>, j: &mut Job, by: i64, invention_successful: i32, completed: i64) -> Result<(), CallError> {
    let bp = match blueprint(db, sde, j.blueprint_id).await {
        Ok(bp) => Some(bp),
        Err(CallError::User { .. }) => None,
        Err(error) => return Err(error),
    };
    let act = sde.industry.activity(j.blueprint_type, j.activity);
    let mut successful = j.runs;
    match (j.activity, &act) {
        (MANUFACTURING, Some(act)) => {
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                let quantity = (per_run * j.runs as i64).min(i32::MAX as i64) as i32;
                deliver(db, sde, outbox, j.owner, j.facility, product, quantity).await?;
            }
            if let Some(bp) = bp.filter(|b| b.copy) {
                let left = bp.runs - j.runs;
                if left <= 0 {
                    destroy_blueprint(db, sde, outbox, bp.item_id).await?;
                } else {
                    set_blueprint(db, bp.item_id, bp.me, bp.te, left).await?;
                }
            }
        }
        (RESEARCH_TIME, _) => {
            if let Some(bp) = bp {
                set_blueprint(db, bp.item_id, bp.me, (bp.te + 2 * j.runs).min(MAX_TIME_EFFICIENCY), bp.runs).await?;
            }
        }
        (RESEARCH_MATERIAL, _) => {
            if let Some(bp) = bp {
                set_blueprint(db, bp.item_id, (bp.me + j.runs).min(MAX_MATERIAL_EFFICIENCY), bp.te, bp.runs).await?;
            }
        }
        (COPYING, _) => {
            let (me, te) = bp.as_ref().map(|b| (b.me, b.te)).unwrap_or((0, 0));
            make_copies(db, sde, outbox, j.owner, j.facility, j.blueprint_type, j.runs, me, te, j.licensed_runs).await?;
        }
        (INVENTION, Some(act)) => {
            successful = invention_successful;
            let (_, d_me, d_te, d_runs) = match j.optional_type {
                Some(d) => decryptor(sde, d),
                None => (1.0, 0, 0, 0),
            };
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                if successful > 0 {
                    let runs = (per_run as i32 + d_runs).max(1);
                    make_copies(db, sde, outbox, j.owner, j.facility, product, successful, INVENTION_MATERIAL_EFFICIENCY + d_me, INVENTION_TIME_EFFICIENCY + d_te, runs).await?;
                }
            }
            if let Some(bp) = bp {
                let left = bp.runs - j.runs;
                if left <= 0 {
                    destroy_blueprint(db, sde, outbox, bp.item_id).await?;
                } else {
                    set_blueprint(db, bp.item_id, bp.me, bp.te, left).await?;
                }
            }
        }
        _ => {}
    }
    j.status = STATUS_DELIVERED;
    j.successful_runs = Some(successful);
    j.completed = Some(completed);
    j.completed_by = Some(by);
    let affected = industry_dao::deliver_job(db, j, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)?;
    if affected == 0 {
        return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
    }
    tell_job(outbox, j);
    Ok(())
}

pub(super) async fn complete(c: &Client, id: i64) -> Result<(), CallError> {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    let j = job(db, id).await?;
    if j.owner != me && j.installer != me {
        return Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![]));
    }
    if j.status != INDUSTRY_JOB_STATUS_INSTALLED {
        return Err(notify_error(messages::INDUSTRY_PROJECT_ALREADY_DELIVERED));
    }
    if j.end > filetime_now() {
        return Err(notify_error(messages::INDUSTRY_PROJECT_NOT_COMPLETE));
    }
    let successful = if j.activity == INVENTION { (0..j.runs).filter(|_| rand01() < j.probability).count() as i32 } else { j.runs };
    let completed = filetime_now();
    let sde = Arc::clone(&c.sde);
    let delivered = db.transaction(
        |tx, outbox: &mut Outbox| {
            let mut j = j.clone();
            let sde = Arc::clone(&sde);
            Box::pin(async move {
                deliver_job(tx, &sde, outbox, &mut j, me, successful, completed).await?;
                Ok(j)
            })
        },
        |j, outbox| async move { dispatch_world(j, outbox, world) },
    ).await?;
    info!("industry: job {} delivered ({}/{} runs)", delivered.id, delivered.successful_runs.unwrap_or(0), delivered.runs);
    Ok(())
}


#[cfg(test)]
mod tests;
