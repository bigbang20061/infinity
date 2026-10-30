//! Industry jobs at NPC stations: manufacturing, research, copying and invention
//! (`industryManager`, `facilityManager`, `industryMonitor`; eve/client/script/industry and
//! the client's industry package, which works out the same numbers to preview a job).
//!
//! Stations with a factory (station service 14) manufacture; stations with a laboratory (15)
//! research, copy and invent. A job takes its blueprint from the facility, its materials from
//! the owner's item hangar there and its cost from their wallet, and delivers to that hangar.
//! The server works out materials, time and cost itself, as the industry package does:
//! materials from the blueprint's ME, time from its TE and the character's industry
//! attributes (services::fit), and cost from the estimated item value (adjusted prices) times
//! the system cost index plus the facility tax, the SCC surcharge and, in high-sec, the
//! security tax. Blueprint data is the client's own (sde_native.blueprints). Corporation jobs
//! aren't supported.

use super::contracts::{field, show_items};
use super::dogma::load_items_db;
use super::fit::{Fit, Key};
use super::market::{debit, deliver, graph, jumps, lock, take_items};
use super::{db_err, keyval, notify_error, rand01, user_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{BLUEPRINT_COPY_QUANTITY, BLUEPRINT_INFINITE_RUNS, FILETIME_DAY, FILETIME_SECOND, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, IX_LOCATION_ID, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use marshal::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Existing server policy for completed industry job history, in days.
const COMPLETED_JOB_HISTORY_DAYS: i64 = 90;

// industry.const
const MANUFACTURING: i32 = 1;
const RESEARCH_TIME: i32 = 3;
const RESEARCH_MATERIAL: i32 = 4;
const COPYING: i32 = 5;
const INVENTION: i32 = 8;
const STATUS_DELIVERED: i32 = 101;
const STATUS_CANCELLED: i32 = 102;
const MAX_MATERIAL_EFFICIENCY: i32 = 10;
const MAX_TIME_EFFICIENCY: i32 = 20;
const COST_PERCENTAGE: f64 = 0.02;
const SCC_SURCHARGE_RATE: f64 = 0.04;
const MAX_RUN_LENGTH: f64 = 2_592_000.0;
const MAX_RUNS_HARD_CAP: i32 = 1_000_000;
const INVENTION_MATERIAL_EFFICIENCY: i32 = 2;
const INVENTION_TIME_EFFICIENCY: i32 = 4;
const INVENTION_SKILL_PROBABILITY: f64 = 1.0 / 30.0;
const RESEARCH_TIMES: [f64; 10] = [
    1.0,
    29.0 / 21.0,
    23.0 / 7.0,
    39.0 / 5.0,
    278.0 / 15.0,
    928.0 / 21.0,
    2200.0 / 21.0,
    5251.0 / 21.0,
    4163.0 / 7.0,
    29660.0 / 21.0,
];
/// industry.DISTANCE_PER_LEVEL
const DISTANCE_PER_LEVEL: [i64; 6] = [0, 5, 10, 15, 20, 25];
const SKILL_SUPPLY_CHAIN_MANAGEMENT: i32 = 24268;
const SKILL_SCIENTIFIC_NETWORKING: i32 = 24270;
/// security.common.const.INDUSTRY_SECURITY_TAX_RATE (high-sec only)
const INDUSTRY_SECURITY_TAX_RATE: f64 = 0.05;

/// What an NPC station's industry service charges on the estimated item value.
const FACILITY_TAX: f64 = 0.0025;
/// The system cost index of every activity everywhere (there is no activity tracking).
const SYSTEM_COST_INDEX: f64 = 0.02;

const STATION_SERVICE_FACTORY: i32 = 14;
const STATION_SERVICE_LABORATORY: i32 = 15;
const GROUP_DECRYPTORS: i32 = 1304;

// dogma attributes
const ATTR_MANUFACTURE_SLOT_LIMIT: i32 = 196;
const ATTR_MANUFACTURE_TIME_MULTIPLIER: i32 = 219;
const ATTR_MANUFACTURING_TIME_RESEARCH_SPEED: i32 = 385;
const ATTR_COPY_SPEED_PERCENT: i32 = 387;
const ATTR_MINERAL_NEED_RESEARCH_SPEED: i32 = 398;
const ATTR_MAX_LABORATORY_SLOTS: i32 = 467;
const ATTR_INVENTION_RESEARCH_SPEED: i32 = 1959;
const ATTR_MANUFACTURE_TIME_PER_LEVEL: i32 = 1982;
const ATTR_INVENTION_PROBABILITY_MULTIPLIER: i32 = 1112;
const ATTR_INVENTION_ME_MODIFIER: i32 = 1113;
const ATTR_INVENTION_TE_MODIFIER: i32 = 1114;
const ATTR_INVENTION_MAX_RUN_MODIFIER: i32 = 1124;

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

async fn activity(db: &(impl Executor + ?Sized), bp_type: i32, activity: i32) -> Result<Option<Activity>, CallError> {
    let Some(name) = activity_name(activity) else { return Ok(None) };
    let key = bp_type.to_string();
    let Some(head) = db
        .query_opt(
            "SELECT (data->'activities'->$2->>'time')::float8, COALESCE((data->>'maxProductionLimit')::int, 1)
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
            "SELECT (e->>'typeID')::int, COALESCE((e->>'quantity')::bigint, 1), COALESCE((e->>'level')::int, 0), COALESCE((e->>'probability')::float8, 1)
             FROM sde_native.blueprints b, jsonb_array_elements(COALESCE(b.data->'activities'->$2->'{part}', '[]')) e WHERE b.key = $1"
        )
    };
    let materials = db.query(&list("materials"), &[&key, &name]).await.map_err(db_err)?;
    let products = db.query(&list("products"), &[&key, &name]).await.map_err(db_err)?;
    let skills = db.query(&list("skills"), &[&key, &name]).await.map_err(db_err)?;
    Ok(Some(Activity {
        time: head.get::<_, Option<f64>>(0).unwrap_or(0.0),
        materials: materials.iter().map(|r| (r.get(0), r.get(1))).collect(),
        products: products.iter().map(|r| (r.get(0), r.get(1), r.get(3))).collect(),
        skills: skills.iter().map(|r| (r.get(0), r.get(2))).collect(),
        max_production_limit: head.get(1),
    }))
}

/// industryCommon.GetBlueprintPrice: the adjusted value of what the blueprint's manufacturing
/// takes, at base quantities (the estimated item value of one run).
async fn blueprint_price(db: &(impl Executor + ?Sized), bp_type: i32) -> Result<f64, CallError> {
    let Some(m) = activity(db, bp_type, MANUFACTURING).await? else { return Ok(0.0) };
    let ids: Vec<i32> = m.materials.iter().map(|(t, _)| *t).collect();
    let rows = db.query("SELECT type_id, adjusted FROM game.retail_prices WHERE type_id = ANY($1)", &[&ids]).await.map_err(db_err)?;
    let price: HashMap<i32, f64> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    Ok(m.materials.iter().map(|(t, q)| price.get(t).copied().unwrap_or(0.0) * *q as f64).sum())
}

/// A blueprint item with its research state.
struct Blueprint {
    item_id: i64,
    type_id: i32,
    owner: i64,
    /// The station it is in (directly or in a container).
    station: Option<i64>,
    singleton: bool,
    quantity: i32,
    copy: bool,
    me: i32,
    te: i32,
    runs: i32,
}

async fn blueprint(db: &(impl Executor + ?Sized), item_id: i64) -> Result<Blueprint, CallError> {
    let r = db
        .query_opt(
            "SELECT i.item_id, i.type_id, i.owner_id, st.station_id::bigint, i.singleton, i.quantity,
                    COALESCE(b.material_efficiency, 0), COALESCE(b.time_efficiency, 0), COALESCE(b.runs, $2)
             FROM game.items i
             LEFT JOIN game.items loc ON loc.item_id = i.location_id
             LEFT JOIN sde.sta_stations st ON st.station_id::bigint = COALESCE(loc.location_id, i.location_id)
             LEFT JOIN game.blueprints b ON b.item_id = i.item_id
             WHERE i.item_id = $1",
            &[&item_id, &BLUEPRINT_INFINITE_RUNS],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| user_error("BlueprintNotFound", vec![]))?;
    let (singleton, quantity): (bool, i32) = (r.get(4), r.get(5));
    Ok(Blueprint {
        item_id: r.get(0),
        type_id: r.get(1),
        owner: r.get(2),
        station: r.get(3),
        singleton,
        quantity,
        copy: singleton && quantity == BLUEPRINT_COPY_QUANTITY as i32,
        me: r.get(6),
        te: r.get(7),
        runs: r.get(8),
    })
}

async fn set_blueprint(db: &(impl Executor + ?Sized), item_id: i64, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    db
        .execute(
            "INSERT INTO game.blueprints (item_id, material_efficiency, time_efficiency, runs) VALUES ($1, $2, $3, $4)
             ON CONFLICT (item_id) DO UPDATE SET material_efficiency = $2, time_efficiency = $3, runs = $4",
            &[&item_id, &me, &te, &runs],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

/// A station's industry facility, if it has a factory or a laboratory.
struct Facility {
    id: i64,
    type_id: i64,
    owner: i64,
    system: i64,
    security: f64,
    factory: bool,
    laboratory: bool,
}

impl Facility {
    fn activities(&self) -> Vec<i32> {
        let mut a = vec![];
        if self.factory {
            a.push(MANUFACTURING);
        }
        if self.laboratory {
            a.extend([RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION]);
        }
        a
    }

    /// industryCommon.Facility data.
    fn data(&self) -> Value {
        let empty = || Value::list([]);
        let activities = self.activities();
        let s = Value::str;
        Value::Dict(vec![
            (s("facilityID"), Value::Int(self.id)),
            (s("typeID"), Value::Int(self.type_id)),
            (s("ownerID"), Value::Int(self.owner)),
            (s("tax"), Value::Float(FACILITY_TAX)),
            (s("solarSystemID"), Value::Int(self.system)),
            (s("online"), Value::Bool(true)),
            (s("serviceAccess"), Value::Dict(vec![])),
            (s("sccTaxModifier"), Value::Float(1.0)),
            (s("rigModifiers"), empty()),
            (
                s("globalModifiers"),
                Value::Dict(vec![
                    (s("faction"), Value::Float(1.0)),
                    (s("system"), Value::Dict(activities.iter().map(|a| (Value::Int(*a as i64), Value::Float(SYSTEM_COST_INDEX))).collect())),
                ]),
            ),
            (
                s("activities"),
                Value::Dict(activities.iter().map(|a| (Value::Int(*a as i64), Value::tuple([empty(), empty(), empty(), empty(), empty(), empty()]))).collect()),
            ),
        ])
    }

    fn location(&self, owner: i64) -> Value {
        Value::reduce(
            "industry.job.Location",
            vec![],
            Some(Value::dict([
                ("itemID", Value::Int(self.id)),
                ("typeID", Value::Int(self.type_id)),
                ("ownerID", Value::Int(owner)),
                ("flagID", Value::Int(FLAG_HANGAR as i64)),
                ("canView", Value::Bool(true)),
                ("canTake", Value::Bool(true)),
            ])),
        )
    }
}

const FACILITY_SELECT: &str = "SELECT s.station_id::bigint, s.type_id::bigint, COALESCE(s.owner_id, 0)::bigint, s.solar_system_id::bigint,
        COALESCE(s.security, 0)::float8, COALESCE(o.data->'services' @> to_jsonb($1::int), false),
        COALESCE(o.data->'services' @> to_jsonb($2::int), false)
    FROM sde.sta_stations s LEFT JOIN sde_native.stationoperations o ON o.key = s.operation_id::text";

async fn facilities(db: &(impl Executor + ?Sized), where_sql: &str, arg: i64) -> Result<Vec<Facility>, CallError> {
    let rows = db.query(&format!("{FACILITY_SELECT} WHERE {where_sql}"), &[&STATION_SERVICE_FACTORY, &STATION_SERVICE_LABORATORY, &arg]).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| Facility { id: r.get(0), type_id: r.get(1), owner: r.get(2), system: r.get(3), security: r.get(4), factory: r.get(5), laboratory: r.get(6) })
        .filter(|f| f.factory || f.laboratory)
        .collect())
}

async fn facility(db: &(impl Executor + ?Sized), id: i64) -> Result<Facility, CallError> {
    facilities(db, "s.station_id = $3::bigint", id).await?.pop().ok_or_else(|| notify_error("该空间站没有工业服务。"))
}

/// The owner's materials in their item hangar at a facility: typeID -> quantity.
async fn available(db: &(impl Executor + ?Sized), owner: i64, station: i64) -> Result<HashMap<i32, i64>, CallError> {
    let rows = db
        .query(
            "SELECT type_id, sum(quantity)::bigint FROM game.items
             WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3 AND NOT singleton GROUP BY type_id",
            &[&owner, &station, &FLAG_HANGAR],
        )
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

#[derive(Clone)]
struct Job {
    id: i64,
    installer: i64,
    owner: i64,
    activity: i32,
    blueprint_id: i64,
    blueprint_type: i32,
    blueprint_copy: bool,
    facility: i64,
    system: i64,
    runs: i32,
    licensed_runs: i32,
    successful_runs: Option<i32>,
    product_type: Option<i32>,
    optional_type: Option<i32>,
    cost: f64,
    probability: f64,
    time: i64,
    start: i64,
    end: i64,
    completed: Option<i64>,
    completed_by: Option<i64>,
    status: i32,
}

const JOB_SELECT: &str = "SELECT job_id, installer_id, owner_id, activity_id, blueprint_id, blueprint_type_id, blueprint_copy, facility_id,
        solar_system_id, runs, licensed_runs, successful_runs, product_type_id, optional_type_id, cost, probability, time_seconds,
        start_date, end_date, completed_date, completed_by, status
    FROM game.industry_jobs";

fn job_from_row(r: &tokio_postgres::Row) -> Job {
    Job {
        id: r.get(0),
        installer: r.get(1),
        owner: r.get(2),
        activity: r.get(3),
        blueprint_id: r.get(4),
        blueprint_type: r.get(5),
        blueprint_copy: r.get(6),
        facility: r.get(7),
        system: r.get(8),
        runs: r.get(9),
        licensed_runs: r.get(10),
        successful_runs: r.get(11),
        product_type: r.get(12),
        optional_type: r.get(13),
        cost: r.get(14),
        probability: r.get(15),
        time: r.get(16),
        start: r.get(17),
        end: r.get(18),
        completed: r.get(19),
        completed_by: r.get(20),
        status: r.get(21),
    }
}

impl Job {
    /// The job data industryCommon.JobData reads.
    fn keyval(&self) -> Value {
        let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
        keyval([
            ("jobID", Value::Int(self.id)),
            ("installerID", Value::Int(self.installer)),
            ("ownerID", Value::Int(self.owner)),
            ("activityID", Value::Int(self.activity as i64)),
            ("blueprintID", Value::Int(self.blueprint_id)),
            ("blueprintTypeID", Value::Int(self.blueprint_type as i64)),
            ("blueprintCopy", Value::Bool(self.blueprint_copy)),
            ("blueprintLocationID", Value::Int(self.facility)),
            ("blueprintLocationFlagID", Value::Int(FLAG_HANGAR as i64)),
            ("facilityID", Value::Int(self.facility)),
            ("stationID", Value::Int(self.facility)),
            ("solarSystemID", Value::Int(self.system)),
            ("outputLocationID", Value::Int(self.facility)),
            ("outputFlagID", Value::Int(FLAG_HANGAR as i64)),
            ("runs", Value::Int(self.runs as i64)),
            ("licensedRuns", Value::Int(self.licensed_runs as i64)),
            ("successfulRuns", opt(self.successful_runs.map(|v| v as i64))),
            ("productTypeID", opt(self.product_type.map(|v| v as i64))),
            ("optionalTypeID", opt(self.optional_type.map(|v| v as i64))),
            ("optionalTypeID2", Value::None),
            ("cost", Value::Float(self.cost)),
            ("probability", Value::Float(self.probability)),
            ("timeInSeconds", Value::Int(self.time)),
            ("startDate", Value::Int(self.start)),
            ("endDate", Value::Int(self.end)),
            ("pauseDate", Value::None),
            ("completedDate", opt(self.completed)),
            ("completedCharacterID", opt(self.completed_by)),
            ("status", Value::Int(self.status as i64)),
        ])
    }
}

async fn job(db: &Db, id: i64) -> Result<Job, CallError> {
    db
        .query_opt(&format!("{JOB_SELECT} WHERE job_id = $1"), &[&id])
        .await
        .map_err(db_err)?
        .map(|r| job_from_row(&r))
        .ok_or_else(|| user_error("IndustryJobNotFound", vec![]))
}

fn tell_job<X>(outbox: &mut Outbox<X>, j: &Job) {
    let successful = j.successful_runs.map(|v| Value::Int(v as i64)).unwrap_or(Value::None);
    let args = vec![Value::Int(j.id), Value::Int(j.owner), Value::Int(j.blueprint_id), Value::Int(j.installer), Value::Int(j.status as i64), successful];
    outbox.notify_pilot(j.installer, "OnIndustryJob", args);
    outbox.notify_pilot(j.owner, "OnBlueprintsUpdated", vec![Value::Int(j.owner)]);
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
    let attr = |a: i32, d: f64| rows.iter().find(|r| r.get::<_, i32>(0) == a).map(|r| r.get::<_, f64>(1)).unwrap_or(d);
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
                    .map(|r| r.get(0));
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
        return Err(notify_error("该项目耗时过长，请减少流程数。"));
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
async fn job_counts(db: &(impl Executor + ?Sized), installer: i64) -> Result<HashMap<i32, i64>, CallError> {
    let rows = db
        .query("SELECT activity_id, count(*) FROM game.industry_jobs WHERE installer_id = $1 AND status = $2 GROUP BY 1", &[&installer, &INDUSTRY_JOB_STATUS_INSTALLED])
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

/// `InstallJob(job.dump())` -> jobID.
async fn install(c: &Client, request: &Value) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let int = |k: &str| request.get(k).filter(|v| !v.is_none()).and_then(|v| v.as_int());
    let activity_id = int("activityID").unwrap_or(0) as i32;
    let runs = int("runs").unwrap_or(1).clamp(1, MAX_RUNS_HARD_CAP as i64) as i32;
    let licensed_runs = int("licensedRuns").unwrap_or(1).max(1) as i32;
    let account_owner = request.get("account").and_then(|v| v.items()).and_then(|a| a.first()).and_then(|v| v.as_int()).unwrap_or(me);
    if account_owner != me {
        return Err(notify_error("暂不支持军团工业项目。"));
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
                    return Err(user_error("IndustryBlueprintAccessDenied", vec![]));
                }
                let busy: bool = tx
                    .query_one("SELECT EXISTS (SELECT 1 FROM game.industry_jobs WHERE blueprint_id = $1 AND status = $2)", &[&bp.item_id, &INDUSTRY_JOB_STATUS_INSTALLED])
                    .await
                    .map_err(db_err)?
                    .get(0);
                if busy {
                    return Err(notify_error("该蓝图正在被一个项目使用。"));
                }
                let fac = facility(tx, facility_id).await?;
                if !fac.activities().contains(&activity_id) {
                    return Err(notify_error("该空间站不提供这项活动。"));
                }
                if bp.station != Some(fac.id) {
                    return Err(notify_error("蓝图必须位于该设施中。"));
                }
                let Some(act) = activity(tx, bp.type_id, activity_id).await? else {
                    return Err(notify_error("该蓝图不能用于这项活动。"));
                };
                // Originals research and copy; only copies are invented from.
                let needs_original = matches!(activity_id, RESEARCH_TIME | RESEARCH_MATERIAL | COPYING);
                if needs_original && bp.copy || activity_id == INVENTION && !bp.copy {
                    return Err(notify_error("该蓝图不能用于这项活动。"));
                }
                let max_runs = match activity_id {
                    RESEARCH_TIME => (MAX_TIME_EFFICIENCY - bp.te) / 2,
                    RESEARCH_MATERIAL => MAX_MATERIAL_EFFICIENCY - bp.me,
                    MANUFACTURING | INVENTION if bp.copy => bp.runs,
                    _ => MAX_RUNS_HARD_CAP,
                };
                if runs > max_runs || (activity_id == COPYING && licensed_runs > act.max_production_limit) {
                    return Err(notify_error("流程数超出了蓝图允许的范围。"));
                }
                let skill_rows = tx.query("SELECT type_id, level FROM game.skills WHERE character_id = $1", &[&me]).await.map_err(db_err)?;
                let skills: HashMap<i32, i32> = skill_rows.iter().map(|r| (r.get(0), r.get(1))).collect();
                if let Some((skill, level)) = act.skills.iter().find(|(s, l)| skills.get(s).copied().unwrap_or(0) < *l) {
                    return Err(notify_error(&format!("该项目需要技能 {skill} 达到 {level} 级。")));
                }
                // Remote jobs need Supply Chain Management / Scientific Networking.
                let reach_skill = if activity_id == MANUFACTURING { SKILL_SUPPLY_CHAIN_MANAGEMENT } else { SKILL_SCIENTIFIC_NETWORKING };
                let reach = DISTANCE_PER_LEVEL[skills.get(&reach_skill).copied().unwrap_or(0).clamp(0, 5) as usize];
                if jumps(system_graph, here, fac.system, reach).is_none() {
                    return Err(notify_error("该设施超出了你的远程工业范围。"));
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
                    return Err(notify_error(&format!("你在这项活动上的全部 {slots} 个工作槽位都已占用。")));
                }
                let optional = if activity_id == INVENTION {
                    let mut decryptor = None;
                    for t in requested.keys() {
                        let group: Option<i32> = tx
                            .query_opt("SELECT group_id FROM sde.inv_types WHERE type_id = $1", &[t])
                            .await
                            .map_err(db_err)?
                            .and_then(|r| r.get(0));
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
                    return Err(notify_error(&format!("你需要在此处物品机库中准备 {q} 个类型 {t}。")));
                }
                debit(tx, outbox, me, p.total_cost).await?;
                for (type_id, quantity) in &p.materials {
                    let mut left = *quantity;
                    let stacks = load_items_db(
                        tx,
                        "i.owner_id = $1 AND i.location_id = $2 AND i.flag_id = $3 AND i.type_id = $4 AND NOT i.singleton ORDER BY i.quantity, i.item_id",
                        &[&me, &fac.id, &FLAG_HANGAR, type_id],
                    )
                    .await?;
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
                    let location: i64 =
                        tx.query_one("SELECT location_id FROM game.items WHERE item_id = $1", &[&bp.item_id]).await.map_err(db_err)?.get(0);
                    if bp.quantity > 1 {
                        let stack = load_items_db(tx, "i.item_id = $1", &[&bp.item_id]).await?.pop().ok_or_else(|| user_error("BlueprintNotFound", vec![]))?;
                        take_items(tx, outbox, stack, 1).await?;
                        bp.item_id = tx
                            .query_one(
                                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
                                &[&bp.type_id, &me, &location, &FLAG_HANGAR],
                            )
                            .await
                            .map_err(db_err)?
                            .get(0);
                        show_items(tx, outbox, me, location, &[bp.item_id], LOCATION_JUNKYARD).await?;
                    } else {
                        tx.execute("UPDATE game.items SET singleton = true WHERE item_id = $1", &[&bp.item_id]).await.map_err(db_err)?;
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
                j.id = tx
                    .query_one(
                        "INSERT INTO game.industry_jobs (installer_id, owner_id, activity_id, blueprint_id, blueprint_type_id, blueprint_copy, facility_id,
                                                         solar_system_id, runs, licensed_runs, product_type_id, optional_type_id, cost, probability,
                                                         time_seconds, start_date, end_date, status)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING job_id",
                        &[
                            &j.installer,
                            &j.owner,
                            &j.activity,
                            &j.blueprint_id,
                            &j.blueprint_type,
                            &j.blueprint_copy,
                            &j.facility,
                            &j.system,
                            &j.runs,
                            &j.licensed_runs,
                            &j.product_type,
                            &j.optional_type,
                            &j.cost,
                            &j.probability,
                            &j.time,
                            &j.start,
                            &j.end,
                            &j.status,
                        ],
                    )
                    .await
                    .map_err(db_err)?
                    .get(0);
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
    if let Some(item) = load_items_db(db, "i.item_id = $1", &[&item_id]).await?.pop() {
        let owner = item.owner_id;
        db.execute("DELETE FROM game.items WHERE item_id = $1", &[&item_id]).await.map_err(db_err)?;
        db.execute("DELETE FROM game.blueprints WHERE item_id = $1", &[&item_id]).await.map_err(db_err)?;
        let old = item.location_id;
        let gone = super::dogma::Item { location_id: LOCATION_JUNKYARD, ..item };
        outbox.notify_pilot(owner, "OnItemChange", vec![gone.row(), Value::Dict(vec![(Value::Int(IX_LOCATION_ID), Value::Int(old))]), Value::None]);
    }
    Ok(())
}

/// New blueprint copies in `owner`'s hangar at `station`.
#[allow(clippy::too_many_arguments)]
async fn make_copies<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, owner: i64, station: i64, type_id: i32, count: i32, me: i32, te: i32, runs: i32) -> Result<(), CallError> {
    let mut ids = vec![];
    for _ in 0..count {
        let id: i64 = db
            .query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5::bigint::integer, true) RETURNING item_id",
                &[&type_id, &owner, &station, &FLAG_HANGAR, &BLUEPRINT_COPY_QUANTITY],
            )
            .await
            .map_err(db_err)?
            .get(0);
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
    let affected = db
        .execute(
            "UPDATE game.industry_jobs SET status = $2, successful_runs = $3, completed_date = $4, completed_by = $5 WHERE job_id = $1 AND status = $6",
            &[&j.id, &j.status, &j.successful_runs, &j.completed, &j.completed_by, &INDUSTRY_JOB_STATUS_INSTALLED],
        )
        .await
        .map_err(db_err)?;
    if affected == 0 {
        return Err(notify_error("该项目已经交付。"));
    }
    tell_job(outbox, j);
    Ok(())
}

async fn complete(c: &Client, id: i64) -> Result<(), CallError> {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let _trading = lock().await;
    let j = job(db, id).await?;
    if j.owner != me && j.installer != me {
        return Err(user_error("IndustryJobAccessDenied", vec![]));
    }
    if j.status != INDUSTRY_JOB_STATUS_INSTALLED {
        return Err(notify_error("该项目已经交付。"));
    }
    if j.end > filetime_now() {
        return Err(notify_error("该项目尚未完成。"));
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

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match (call.service, call.method) {
        ("facilityManager", "GetFacilities") => {
            let region = c.session.int("regionid").unwrap_or(0);
            facilities(&c.db, "s.region_id = $3::bigint", region).await.map(|f| Value::list(f.iter().map(Facility::data)))
        }
        ("facilityManager", "GetFacility") => facility(&c.db, int(0)).await.map(|f| f.data()),
        ("facilityManager", "GetFacilitiesByID") => {
            let ids = value_id_list(call.args.first());
            let mut out = vec![];
            for id in ids {
                match facility(&c.db, id).await {
                    Ok(f) => out.push(f.data()),
                    Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::List(out))
        }
        // GetFacilityLocations(facilityID, ownerID): the owner's item hangar there.
        ("facilityManager", "GetFacilityLocations") => {
            let owner = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(me);
            match facility(&c.db, int(0)).await {
                Ok(f) if owner == me => Ok(Value::list([f.location(me)])),
                Ok(_) | Err(CallError::User { .. }) => Ok(Value::list([])),
                Err(e) => Err(e),
            }
        }
        ("facilityManager", "GetMaxActivityModifiers") => {
            Ok(Value::Dict([MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION].iter().map(|a| (Value::Int(*a as i64), Value::Float(1.0))).collect()))
        }
        ("facilityManager", "GetFacilityTaxes") => Ok(Value::Dict(vec![])),
        ("facilityManager", "SetFacilityTaxes") => Ok(Value::None),

        // ConnectJob(job.dump()) -> (monitorID, {typeID: quantity available})
        ("industryMonitor", "ConnectJob") => {
            let request = call.args.first().cloned().unwrap_or(Value::None);
            let station = request.get("inputLocation").and_then(|l| field(l, "itemID")).and_then(|v| v.as_int()).or_else(|| request.get("facilityID").and_then(|v| v.as_int()));
            let have = match station {
                Some(s) => match available(&c.db, me, s).await {
                    Ok(h) => h,
                    Err(e) => return Some(Err(e)),
                },
                None => HashMap::new(),
            };
            Ok(Value::tuple([Value::Int(1), Value::Dict(have.into_iter().map(|(t, q)| (Value::Int(t as i64), Value::Int(q))).collect())]))
        }
        ("industryMonitor", "DisconnectJob") => Ok(Value::None),

        ("industryManager", "InstallJob") => match call.args.first() {
            Some(request) => install(c, request).await,
            None => Err(notify_error("没有要安装的项目。")),
        },
        // GetJobsByOwner(ownerID, includeCompleted)
        ("industryManager", "GetJobsByOwner") => {
            let owner = int(0);
            let completed = call.args.get(1).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0));
            let rows = if completed {
                c.db
                    .query(&format!("{JOB_SELECT} WHERE owner_id = $1 AND (status = $2 OR completed_date >= $3) ORDER BY job_id"), &[&owner, &INDUSTRY_JOB_STATUS_INSTALLED, &(filetime_now() - COMPLETED_JOB_HISTORY_DAYS * FILETIME_DAY)])
                    .await
            } else {
                c.db.query(&format!("{JOB_SELECT} WHERE owner_id = $1 AND status = $2 ORDER BY job_id"), &[&owner, &INDUSTRY_JOB_STATUS_INSTALLED]).await
            };
            match rows {
                Ok(rows) => Ok(Value::list(rows.iter().map(|r| job_from_row(r).keyval()))),
                Err(e) => Err(db_err(e)),
            }
        }
        ("industryManager", "GetJob") => job(&c.db, int(0)).await.map(|j| j.keyval()),
        ("industryManager", "GetJobCounts") => {
            job_counts(&c.db, int(0)).await.map(|counts| Value::Dict(counts.into_iter().map(|(a, n)| (Value::Int(a as i64), Value::Int(n))).collect()))
        }
        // CompleteJob(jobID, solarSystemID)
        ("industryManager", "CompleteJob") => complete(c, int(0)).await.map(|_| Value::None),
        // CompleteManyJobs([(jobID, solarSystemID)])
        ("industryManager", "CompleteManyJobs") => {
            let ids: Vec<i64> = call
                .args
                .first()
                .and_then(|v| v.items())
                .unwrap_or(&[])
                .iter()
                .filter_map(|p| p.items().and_then(|p| p.first()).and_then(|v| v.as_int()))
                .collect();
            for id in ids {
                match complete(c, id).await {
                    Ok(()) | Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::None)
        }
        // CancelJob(jobID, solarSystemID): the materials and cost are lost.
        ("industryManager", "CancelJob") => {
            let _trading = lock().await;
            match job(&c.db, int(0)).await {
                Ok(mut j) if (j.owner == me || j.installer == me) && j.status == INDUSTRY_JOB_STATUS_INSTALLED => {
                    j.status = STATUS_CANCELLED;
                    j.completed = Some(filetime_now());
                    j.completed_by = Some(me);
                    match c
                        .db
                        .execute("UPDATE game.industry_jobs SET status = $2, completed_date = $3, completed_by = $4 WHERE job_id = $1", &[&j.id, &j.status, &j.completed, &me])
                        .await
                    {
                        Ok(_) => {
                            info!("industry: {me} cancelled job {}", j.id);
                            let mut outbox = Outbox::default();
                            tell_job(&mut outbox, &j);
                            dispatch_world(Value::None, outbox, &c.world)
                        }
                        Err(e) => Err(db_err(e)),
                    }
                }
                Ok(_) => Err(notify_error("该项目无法取消。")),
                Err(e) => Err(e),
            }
        }
        _ => return None,
    })
}
