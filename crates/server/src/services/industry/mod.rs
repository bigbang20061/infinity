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
//! security tax. These rates, invention skill probability, run limits and completed-job history
//! are operator policies in the industry configuration. Blueprint and station-operation data
//! comes from the immutable typed SDE. Corporation jobs aren't supported.
//! NPC facility tax settings are read-only; submitted cost and tax quotes are checked against
//! the same configured tax and system index used for settlement.

use super::contracts::{field, show_items};
use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use crate::dao::industry::{self as industry_dao, Blueprint, Job};
use super::fit::{Fit, Key};
use super::items::{deliver, take_items};
use super::market::{jumps, lock};
use super::wallets::debit;
use super::{db_err, keyval, notify_error, rand01, user_error, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{BLUEPRINT_COPY_QUANTITY, FILETIME_DAY, FILETIME_SECOND, FLAG_HANGAR, INDUSTRY_JOB_STATUS_INSTALLED, IX_LOCATION_ID, LOCATION_JUNKYARD};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::sde::industry::{MANUFACTURING, RESEARCH_TIME, RESEARCH_MATERIAL, COPYING, INVENTION};
use marshal::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;


// industry.const
const STATUS_DELIVERED: i32 = 101;
const STATUS_CANCELLED: i32 = 102;
const MAX_MATERIAL_EFFICIENCY: i32 = 10;
const MAX_TIME_EFFICIENCY: i32 = 20;
const INVENTION_MATERIAL_EFFICIENCY: i32 = 2;
const INVENTION_TIME_EFFICIENCY: i32 = 4;
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
    fn from_station(sde: &crate::sde::Sde, station: &crate::sde::map::Station) -> Option<Self> {
        let factory = sde.industry.has_service(station.operation_id, STATION_SERVICE_FACTORY);
        let laboratory = sde.industry.has_service(station.operation_id, STATION_SERVICE_LABORATORY);
        Self::from_station_services(station, factory, laboratory)
    }

    fn from_station_services(station: &crate::sde::map::Station, factory: bool, laboratory: bool) -> Option<Self> {
        let type_id = station.type_id.expect("SDE industry station has no type_id");
        let system = station.solar_system_id.expect("SDE industry station has no solar_system_id");
        if !factory && !laboratory { return None; }
        Some(Self { id: station.station_id, type_id, owner: station.owner_id.unwrap_or(0),
            system, security: station.security.unwrap_or(0.0), factory, laboratory })
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

    fn tax(&self) -> f64 {
        crate::config::game().industry.npc_facility_tax
    }

    fn system_index(&self) -> f64 {
        crate::config::game().industry.system_cost_index
    }

    fn tax_settings(&self) -> Value {
        keyval([
            ("taxCorporation", Value::Float(self.tax())),
            ("taxAlliance", Value::None),
            ("taxStandingsHorrible", Value::None),
            ("taxStandingsBad", Value::None),
            ("taxStandingsNeutral", Value::None),
            ("taxStandingsGood", Value::None),
            ("taxStandingsHigh", Value::None),
        ])
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
            (s("tax"), Value::Float(self.tax())),
            (s("solarSystemID"), Value::Int(self.system)),
            (s("online"), Value::Bool(true)),
            (s("serviceAccess"), Value::Dict(vec![])),
            (s("sccTaxModifier"), Value::Float(1.0)),
            (s("rigModifiers"), empty()),
            (
                s("globalModifiers"),
                Value::Dict(vec![
                    (s("faction"), Value::Float(1.0)),
                    (s("system"), Value::Dict(activities.iter().map(|a| (Value::Int(*a as i64), Value::Float(self.system_index()))).collect())),
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

fn facility(sde: &crate::sde::Sde, id: i64) -> Result<Facility, CallError> {
    sde.map.station(id).and_then(|s| Facility::from_station(sde, s))
        .ok_or_else(|| notify_error(messages::NO_INDUSTRIAL_SERVICES))
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

#[cfg(test)]
mod tests;

