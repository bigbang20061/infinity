//! Industry job calculations and lifecycle for the client industry package.
//! Uses blueprint SDE data and game jobs, items and wallets; corporation jobs are unsupported.

use super::*;
fn activity_name(activity: i32) -> Option<&'static str> {
    Some(match activity {
        MANUFACTURING => "manufacturing",
        RESEARCH_TIME => "research_time",
        RESEARCH_MATERIAL => "research_material",
        COPYING => "copying",
        INVENTION => "invention",
        _ => return None,
    })
}

/// A blueprint type's activity (sde_native.blueprints `activities.<name>`).
struct Activity {
    time: f64,
    materials: Vec<(i32, i64)>,
    /// (typeID, quantity, probability)
    products: Vec<(i32, i64, f64)>,
    skills: Vec<(i32, i32)>,
    max_production_limit: i32,
}

struct ActivityHeadRow {
    time: Option<f64>,
    max_production_limit: i32,
}

impl ActivityHeadRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { time: row.get("time"), max_production_limit: row.get("max_production_limit") }
    }
}

struct ActivityPartRow {
    type_id: i32,
    quantity: i64,
    level: i32,
    probability: f64,
}

impl ActivityPartRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { type_id: row.get("type_id"), quantity: row.get("quantity"), level: row.get("level"), probability: row.get("probability") }
    }
}

async fn activity(db: &(impl Executor + ?Sized), bp_type: i32, activity: i32) -> Result<Option<Activity>, CallError> {
    let Some(name) = activity_name(activity) else { return Ok(None) };
    let key = bp_type.to_string();
    let Some(head) = db
        .query_opt(
            "SELECT (data->'activities'->$2->>'time')::float8 AS time, COALESCE((data->>'maxProductionLimit')::int, 1) AS max_production_limit
             FROM sde_native.blueprints WHERE key = $1 AND data->'activities' ? $2",
            &[&key, &name],
        )
        .await
        .map_err(db_err)?
    else {
        return Ok(None);
    };
    let list = |part: &str| {
        format!(
            "SELECT (e->>'typeID')::int AS type_id, COALESCE((e->>'quantity')::bigint, 1) AS quantity, COALESCE((e->>'level')::int, 0) AS level, COALESCE((e->>'probability')::float8, 1) AS probability
             FROM sde_native.blueprints b, jsonb_array_elements(COALESCE(b.data->'activities'->$2->'{part}', '[]')) e WHERE b.key = $1"
        )
    };
    let materials = db.query(&list("materials"), &[&key, &name]).await.map_err(db_err)?;
    let products = db.query(&list("products"), &[&key, &name]).await.map_err(db_err)?;
    let skills = db.query(&list("skills"), &[&key, &name]).await.map_err(db_err)?;
    let head = ActivityHeadRow::from_row(&head);
    Ok(Some(Activity {
        time: head.time.unwrap_or(0.0),
        materials: materials.iter().map(ActivityPartRow::from_row).map(|r| (r.type_id, r.quantity)).collect(),
        products: products.iter().map(ActivityPartRow::from_row).map(|r| (r.type_id, r.quantity, r.probability)).collect(),
        skills: skills.iter().map(ActivityPartRow::from_row).map(|r| (r.type_id, r.level)).collect(),
        max_production_limit: head.max_production_limit,
    }))
}

/// industryCommon.GetBlueprintPrice: the adjusted value of what the blueprint's manufacturing
/// takes, at base quantities (the estimated item value of one run).
async fn blueprint_price(db: &(impl Executor + ?Sized), bp_type: i32) -> Result<f64, CallError> {
    let Some(m) = activity(db, bp_type, MANUFACTURING).await? else { return Ok(0.0) };
    let ids: Vec<i32> = m.materials.iter().map(|(t, _)| *t).collect();
    let price = industry_dao::prices(db, &ids).await.map_err(db_err)?;
    Ok(m.materials.iter().map(|(t, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum())
}

async fn blueprint(db: &(impl Executor + ?Sized), item_id: i64) -> Result<Blueprint, CallError> {
    industry_dao::blueprint(db, item_id).await.map_err(db_err)?
        .ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))
}

async fn set_blueprint(db: &(impl Executor + ?Sized), item_id: i64, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    industry_dao::set_blueprint(db, item_id, me, te, runs).await.map_err(db_err)
}

struct AttributeRow {
    attribute_id: i32,
    value: f64,
}

impl AttributeRow {
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { attribute_id: r.get("attribute_id"), value: r.get("value") }
    }
}

struct ValueRow { value: f64 }
impl ValueRow {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } }
}

struct GroupRow { group_id: Option<i32> }
impl GroupRow {
    fn from_row(r: &tokio_postgres::Row) -> Self { Self { group_id: r.get("group_id") } }
}

/// Decryptor bonuses (inventionPropabilityMultiplier, ME, TE, max runs) of an invention input.
async fn decryptor(db: &(impl Executor + ?Sized), type_id: i32) -> Result<(f64, i32, i32, i32), CallError> {
    let rows = db
        .query(
            "SELECT attribute_id, value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = ANY($2)",
            &[&type_id, &vec![ATTR_INVENTION_PROBABILITY_MULTIPLIER, ATTR_INVENTION_ME_MODIFIER, ATTR_INVENTION_TE_MODIFIER, ATTR_INVENTION_MAX_RUN_MODIFIER]],
        )
        .await
        .map_err(db_err)?;
    let attr = |a: i32, d: f64| rows.iter().find_map(|r| {
        let r = AttributeRow::from_row(r);
        (r.attribute_id == a).then_some(r.value)
    }).unwrap_or(d);
    Ok((
        attr(ATTR_INVENTION_PROBABILITY_MULTIPLIER, 1.0),
        attr(ATTR_INVENTION_ME_MODIFIER, 0.0) as i32,
        attr(ATTR_INVENTION_TE_MODIFIER, 0.0) as i32,
        attr(ATTR_INVENTION_MAX_RUN_MODIFIER, 0.0) as i32,
    ))
}

/// What a job will take, how long and what it costs, as the industry package computes it.
struct Plan {
    materials: Vec<(i32, i64)>,
    seconds: i64,
    total_cost: f64,
    probability: f64,
    product: Option<i32>,
}

#[allow(clippy::too_many_arguments)]
async fn plan(
    db: &(impl Executor + ?Sized),
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
        let skill_bonus = 1.0 + act.skills.iter().map(|(s, _)| level(*s) * INVENTION_SKILL_PROBABILITY).sum::<f64>();
        let decryptor_multiplier = match optional {
            Some(d) => decryptor(db, d).await?.0,
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
                let per_level: Option<f64> = db
                    .query_opt("SELECT value FROM sde.type_attributes WHERE type_id = $1 AND attribute_id = $2", &[skill, &ATTR_MANUFACTURE_TIME_PER_LEVEL])
                    .await
                    .map_err(db_err)?
                    .map(|r| ValueRow::from_row(&r).value);
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
    if seconds - seconds / runs as f64 > MAX_RUN_LENGTH {
        return Err(notify_error(messages::INDUSTRY_PROJECT_TOO_LONG));
    }

    // Cost: the estimated item value, scaled per activity.
    let product = match activity_id {
        MANUFACTURING | INVENTION => {
            let products: Vec<i32> = act.products.iter().map(|p| p.0).collect();
            requested_product.filter(|p| products.contains(p)).or(products.first().copied())
        }
        _ => None,
    };
    let job_cost = match activity_id {
        MANUFACTURING => blueprint_price(db, bp.type_id).await? * runs as f64,
        COPYING => blueprint_price(db, bp.type_id).await? * COST_PERCENTAGE * licensed_runs as f64 * runs as f64,
        RESEARCH_TIME | RESEARCH_MATERIAL if act.time > 0.0 => blueprint_price(db, bp.type_id).await? * COST_PERCENTAGE * base_seconds / act.time,
        INVENTION => match product {
            Some(p) => blueprint_price(db, p).await? * COST_PERCENTAGE * runs as f64,
            None => 0.0,
        },
        _ => 0.0,
    };
    let base_cost = job_cost.round();
    let scc = if matches!(activity_id, RESEARCH_TIME | RESEARCH_MATERIAL) { SCC_SURCHARGE_RATE * 0.5 } else { SCC_SURCHARGE_RATE };
    let security_tax = if facility.security >= 0.45 { (base_cost * INDUSTRY_SECURITY_TAX_RATE).round() } else { 0.0 };
    let total_cost = (job_cost * SYSTEM_COST_INDEX).round() + (base_cost * FACILITY_TAX).round() + (base_cost * scc).round() + security_tax;
    Ok(Plan { materials, seconds: seconds.round() as i64, total_cost, probability, product })
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
    let runs = int("runs").unwrap_or(1).clamp(1, MAX_RUNS_HARD_CAP as i64) as i32;
    let licensed_runs = int("licensedRuns").unwrap_or(1).max(1) as i32;
    let account_owner = request.get("account").and_then(|v| v.items()).and_then(|a| a.first()).and_then(|v| v.as_int()).unwrap_or(me);
    if account_owner != me {
        return Err(notify_error(messages::CORPORATION_INDUSTRY_UNSUPPORTED));
    }
    let requested: HashMap<i32, i64> = match request.get("materials") {
        Some(Value::Dict(pairs)) => pairs.iter().filter_map(|(k, v)| Some((k.as_int()? as i32, v.as_int().unwrap_or(0)))).collect(),
        _ => HashMap::new(),
    };
    let _trading = lock().await;
    let blueprint_id = int("blueprintID").unwrap_or(0);
    let facility_id = int("facilityID").unwrap_or(0);
    let product_request = int("productTypeID").map(|p| p as i32);
    let here = c.session.int("solarsystemid2").unwrap_or(0);
    let ship = c.session.int("shipid").unwrap_or(0);
    let system_graph = graph(db).await?;
    let fit = Arc::new(Fit::load(db, me, ship).await?);
    let now = filetime_now();
    let j = db.transaction(
        |tx, outbox: &mut Outbox| {
            let requested = requested.clone();
            let fit = Arc::clone(&fit);
            Box::pin(async move {
                let mut bp = blueprint(tx, blueprint_id).await?;
                if bp.owner != me {
                    return Err(user_error(messages::MSG_INDUSTRY_BLUEPRINT_ACCESS_DENIED, vec![]));
                }
                let busy = industry_dao::blueprint_busy(tx, bp.item_id, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)?;
                if busy {
                    return Err(notify_error(messages::BLUEPRINT_IN_USE));
                }
                let fac = facility(tx, facility_id).await?;
                if !fac.activities().contains(&activity_id) {
                    return Err(notify_error(messages::INDUSTRY_ACTIVITY_UNAVAILABLE));
                }
                if bp.station != Some(fac.id) {
                    return Err(notify_error(messages::BLUEPRINT_MUST_BE_AT_FACILITY));
                }
                let Some(act) = activity(tx, bp.type_id, activity_id).await? else {
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
                    _ => MAX_RUNS_HARD_CAP,
                };
                if runs > max_runs || (activity_id == COPYING && licensed_runs > act.max_production_limit) {
                    return Err(notify_error(messages::BLUEPRINT_RUN_COUNT_EXCEEDED));
                }
                let skills = industry_dao::skills(tx, me).await.map_err(db_err)?;
                if let Some((skill, level)) = act.skills.iter().find(|(s, l)| skills.get(s).copied().unwrap_or(0) < *l) {
                    return Err(notify_error(&messages::required_skill_message(skill, level)));
                }
                // Remote jobs need Supply Chain Management / Scientific Networking.
                let reach_skill = if activity_id == MANUFACTURING { SKILL_SUPPLY_CHAIN_MANAGEMENT } else { SKILL_SCIENTIFIC_NETWORKING };
                let reach = DISTANCE_PER_LEVEL[skills.get(&reach_skill).copied().unwrap_or(0).clamp(0, 5) as usize];
                if jumps(system_graph, here, fac.system, reach).is_none() {
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
                        let group: Option<i32> = tx
                            .query_opt("SELECT group_id FROM sde.inv_types WHERE type_id = $1", &[t])
                            .await
                            .map_err(db_err)?
                            .and_then(|r| GroupRow::from_row(&r).group_id);
                        if group == Some(GROUP_DECRYPTORS) {
                            decryptor = Some(*t);
                        }
                    }
                    decryptor
                } else {
                    None
                };
                let p = plan(tx, &fit, &skills, &bp, &act, activity_id, runs, licensed_runs, product_request, optional, &fac).await?;

                let have = available(tx, me, fac.id).await?;
                if let Some((t, q)) = p.materials.iter().find(|(t, q)| have.get(t).copied().unwrap_or(0) < *q) {
                    return Err(notify_error(&messages::required_materials_message(q, t)));
                }
                debit(tx, outbox, me, p.total_cost).await?;
                for (type_id, quantity) in &p.materials {
                    let mut left = *quantity;
                    let stacks = load_items(tx, ItemQuery::OwnerLocationFlagTypeStackAscending(me, fac.id, FLAG_HANGAR, *type_id)).await.map_err(db_err)?;
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
                        let stack = load_items(tx, ItemQuery::Id(bp.item_id)).await.map_err(db_err)?.pop().ok_or_else(|| user_error(messages::MSG_BLUEPRINT_NOT_FOUND, vec![]))?;
                        take_items(tx, outbox, stack, 1).await?;
                        bp.item_id = crate::dao::items::creation::rookie_ship(tx, bp.type_id, me, location, FLAG_HANGAR).await.map_err(db_err)?;
                        show_items(tx, outbox, me, location, &[bp.item_id], LOCATION_JUNKYARD).await?;
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
                    cost: p.total_cost,
                    probability: p.probability,
                    time: p.seconds,
                    start: now,
                    end: now + p.seconds * FILETIME_SECOND,
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
async fn destroy_blueprint<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, item_id: i64) -> Result<(), CallError> {
    if let Some(item) = load_items(db, ItemQuery::Id(item_id)).await.map_err(db_err)?.pop() {
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
async fn make_copies<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, count: i32, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    let mut ids = vec![];
    for _ in 0..count {
        let id = industry_dao::insert_copy(db, type_id, owner, station, FLAG_HANGAR, BLUEPRINT_COPY_QUANTITY).await.map_err(db_err)?;
        set_blueprint(db, id, me, te, runs).await?;
        ids.push(id);
    }
    show_items(db, outbox, owner, station, &ids, LOCATION_JUNKYARD).await
}

/// Deliver a finished job's output to the owner's hangar at the facility.
async fn deliver_job<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, j: &mut Job, by: i64, invention_successful: i32, completed: i64) -> Result<(), CallError> {
    let bp = match blueprint(db, j.blueprint_id).await {
        Ok(bp) => Some(bp),
        Err(CallError::User { .. }) => None,
        Err(error) => return Err(error),
    };
    let act = activity(db, j.blueprint_type, j.activity).await?;
    let mut successful = j.runs;
    match (j.activity, &act) {
        (MANUFACTURING, Some(act)) => {
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                let quantity = (per_run * j.runs as i64).min(i32::MAX as i64) as i32;
                deliver(db, outbox, j.owner, j.facility, product, quantity).await?;
            }
            if let Some(bp) = bp.filter(|b| b.copy) {
                let left = bp.runs - j.runs;
                if left <= 0 {
                    destroy_blueprint(db, outbox, bp.item_id).await?;
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
            make_copies(db, outbox, j.owner, j.facility, j.blueprint_type, j.runs, me, te, j.licensed_runs).await?;
        }
        (INVENTION, Some(act)) => {
            successful = invention_successful;
            let (_, d_me, d_te, d_runs) = match j.optional_type {
                Some(d) => decryptor(db, d).await?,
                None => (1.0, 0, 0, 0),
            };
            if let Some(&(product, per_run, _)) = act.products.iter().find(|p| Some(p.0) == j.product_type).or(act.products.first()) {
                if successful > 0 {
                    let runs = (per_run as i32 + d_runs).max(1);
                    make_copies(db, outbox, j.owner, j.facility, product, successful, INVENTION_MATERIAL_EFFICIENCY + d_me, INVENTION_TIME_EFFICIENCY + d_te, runs).await?;
                }
            }
            if let Some(bp) = bp {
                let left = bp.runs - j.runs;
                if left <= 0 {
                    destroy_blueprint(db, outbox, bp.item_id).await?;
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
    let delivered = db.transaction(
        |tx, outbox: &mut Outbox| {
            let mut j = j.clone();
            Box::pin(async move {
                deliver_job(tx, outbox, &mut j, me, successful, completed).await?;
                Ok(j)
            })
        },
        |j, outbox| async move { dispatch_world(j, outbox, world) },
    ).await?;
    info!("industry: job {} delivered ({}/{} runs)", delivered.id, delivered.successful_runs.unwrap_or(0), delivered.runs);
    Ok(())
}

