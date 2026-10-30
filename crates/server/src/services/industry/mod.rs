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
use crate::dao::items::loading::{load_items, Item, ItemQuery};
use crate::dao::industry::{self as industry_dao, Blueprint, Job};
use super::fit::{Fit, Key};
use super::items::{deliver, take_items};
use super::market::{graph, jumps, lock};
use super::wallets::debit;
use super::{db_err, keyval, notify_error, rand01, user_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{BLUEPRINT_COPY_QUANTITY, FILETIME_DAY, FILETIME_SECOND, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, IX_LOCATION_ID, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
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
    fn from_row(r: &tokio_postgres::Row) -> Self {
        Self { id: r.get("station_id"), type_id: r.get("type_id"), owner: r.get("owner_id"), system: r.get("solar_system_id"), security: r.get("security"), factory: r.get("factory"), laboratory: r.get("laboratory") }
    }

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

const FACILITY_SELECT: &str = "SELECT s.station_id::bigint, s.type_id::bigint, COALESCE(s.owner_id, 0)::bigint AS owner_id, s.solar_system_id::bigint,
        COALESCE(s.security, 0)::float8 AS security, COALESCE(o.data->'services' @> to_jsonb($1::int), false) AS factory,
        COALESCE(o.data->'services' @> to_jsonb($2::int), false) AS laboratory
    FROM sde.sta_stations s LEFT JOIN sde_native.stationoperations o ON o.key = s.operation_id::text";

async fn facilities(db: &(impl Executor + ?Sized), where_sql: &str, arg: i64) -> Result<Vec<Facility>, CallError> {
    let rows = db.query(&format!("{FACILITY_SELECT} WHERE {where_sql}"), &[&STATION_SERVICE_FACTORY, &STATION_SERVICE_LABORATORY, &arg]).await.map_err(db_err)?;
    Ok(rows
        .iter()
        .map(Facility::from_row)
        .filter(|f| f.factory || f.laboratory)
        .collect())
}

async fn facility(db: &(impl Executor + ?Sized), id: i64) -> Result<Facility, CallError> {
    facilities(db, "s.station_id = $3::bigint", id).await?.pop().ok_or_else(|| notify_error(messages::NO_INDUSTRIAL_SERVICES))
}

/// The owner's materials in their item hangar at a facility: typeID -> quantity.
async fn available(db: &(impl Executor + ?Sized), owner: i64, station: i64) -> Result<HashMap<i32, i64>, CallError> {
    industry_dao::available(db, owner, station, FLAG_HANGAR).await.map_err(db_err)
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
    industry_dao::job(db, id).await.map_err(db_err)?
        .ok_or_else(|| user_error(messages::MSG_INDUSTRY_JOB_NOT_FOUND, vec![]))
}

fn tell_job<X>(outbox: &mut Outbox<X>, j: &Job) {
    let successful = j.successful_runs.map(|v| Value::Int(v as i64)).unwrap_or(Value::None);
    let args = vec![Value::Int(j.id), Value::Int(j.owner), Value::Int(j.blueprint_id), Value::Int(j.installer), Value::Int(j.status as i64), successful];
    outbox.notify_pilot(j.installer, "OnIndustryJob", args);
    outbox.notify_pilot(j.owner, "OnBlueprintsUpdated", vec![Value::Int(j.owner)]);
}

mod facility_manager;
mod industry_manager;
mod industry_monitor;
mod mechanics;

use mechanics::{complete, install, job_counts};

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "facilityManager" => facility_manager::dispatch(c, call).await,
        "industryManager" => industry_manager::dispatch(c, call).await,
        "industryMonitor" => industry_monitor::dispatch(c, call).await,
        _ => None,
    }
}
