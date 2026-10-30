//! Industry persistence for blueprints, jobs and materials.
//! The owning services support `eve/client/script/industry`; this DAO has no wire contract.
//! Uses `game.items`, `game.blueprints`, `game.industry_jobs`, `game.retail_prices`
//! and `sde.sta_stations`. Corporation jobs are unsupported by the services;
//! policy, transactions and notifications remain with callers. Cancellation is guarded by installed status.

use std::collections::HashMap;
use crate::consts::{BLUEPRINT_COPY_QUANTITY, BLUEPRINT_INFINITE_RUNS};
use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

const BLUEPRINT_SQL: &str = "SELECT i.item_id, i.type_id, i.owner_id, st.station_id::bigint, i.singleton, i.quantity,
                    COALESCE(b.material_efficiency, 0) AS material_efficiency, COALESCE(b.time_efficiency, 0) AS time_efficiency, COALESCE(b.runs, $2) AS runs
             FROM game.items i
             LEFT JOIN game.items loc ON loc.item_id = i.location_id
             LEFT JOIN sde.sta_stations st ON st.station_id::bigint = COALESCE(loc.location_id, i.location_id)
             LEFT JOIN game.blueprints b ON b.item_id = i.item_id
             WHERE i.item_id = $1";
const SET_BLUEPRINT_SQL: &str = "INSERT INTO game.blueprints (item_id, material_efficiency, time_efficiency, runs) VALUES ($1, $2, $3, $4)
             ON CONFLICT (item_id) DO UPDATE SET material_efficiency = $2, time_efficiency = $3, runs = $4";
const JOB_SELECT: &str = "SELECT job_id, installer_id, owner_id, activity_id, blueprint_id, blueprint_type_id, blueprint_copy, facility_id,
        solar_system_id, runs, licensed_runs, successful_runs, product_type_id, optional_type_id, cost, probability, time_seconds,
        start_date, end_date, completed_date, completed_by, status
    FROM game.industry_jobs";
const COUNTS_SQL: &str = "SELECT activity_id, count(*) AS count FROM game.industry_jobs WHERE installer_id = $1 AND status = $2 GROUP BY 1";
const PRICES_SQL: &str = "SELECT type_id, adjusted FROM game.retail_prices WHERE type_id = ANY($1)";
const AVAILABLE_SQL: &str = "SELECT type_id, sum(quantity)::bigint AS quantity FROM game.items
             WHERE owner_id = $1 AND location_id = $2 AND flag_id = $3 AND NOT singleton GROUP BY type_id";
const BUSY_SQL: &str = "SELECT EXISTS (SELECT 1 FROM game.industry_jobs WHERE blueprint_id = $1 AND status = $2) AS exists";
const LOCATION_SQL: &str = "SELECT location_id FROM game.items WHERE item_id = $1";
const MAKE_SINGLETON_SQL: &str = "UPDATE game.items SET singleton = true WHERE item_id = $1";
const INSERT_JOB_SQL: &str = "INSERT INTO game.industry_jobs (installer_id, owner_id, activity_id, blueprint_id, blueprint_type_id, blueprint_copy, facility_id,
                                                         solar_system_id, runs, licensed_runs, product_type_id, optional_type_id, cost, probability,
                                                         time_seconds, start_date, end_date, status)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING job_id";
const DELETE_BLUEPRINT_SQL: &str = "DELETE FROM game.blueprints WHERE item_id = $1";
const INSERT_COPY_SQL: &str = "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton) VALUES ($1, $2, $3, $4, $5::bigint::integer, true) RETURNING item_id";
const DELIVER_SQL: &str = "UPDATE game.industry_jobs SET status = $2, successful_runs = $3, completed_date = $4, completed_by = $5 WHERE job_id = $1 AND status = $6";
const CANCEL_SQL: &str = "UPDATE game.industry_jobs SET status = $2, completed_date = $3, completed_by = $4 WHERE job_id = $1 AND status = $5";

pub(crate) struct Blueprint {
    pub(crate) item_id: i64, pub(crate) type_id: i32, pub(crate) owner: i64, pub(crate) station: Option<i64>,
    pub(crate) singleton: bool, pub(crate) quantity: i32, pub(crate) copy: bool,
    pub(crate) me: i32, pub(crate) te: i32, pub(crate) runs: i32,
}
impl Blueprint {
    fn from_row(r: &Row) -> Self {
        let singleton = r.get("singleton");
        let quantity = r.get("quantity");
        Self { item_id: r.get("item_id"), type_id: r.get("type_id"), owner: r.get("owner_id"), station: r.get("station_id"),
            singleton, quantity, copy: singleton && quantity == BLUEPRINT_COPY_QUANTITY as i32,
            me: r.get("material_efficiency"), te: r.get("time_efficiency"), runs: r.get("runs") }
    }
}
#[derive(Clone)]
pub(crate) struct Job {
    pub(crate) id: i64, pub(crate) installer: i64, pub(crate) owner: i64, pub(crate) activity: i32,
    pub(crate) blueprint_id: i64, pub(crate) blueprint_type: i32, pub(crate) blueprint_copy: bool,
    pub(crate) facility: i64, pub(crate) system: i64, pub(crate) runs: i32, pub(crate) licensed_runs: i32,
    pub(crate) successful_runs: Option<i32>, pub(crate) product_type: Option<i32>, pub(crate) optional_type: Option<i32>,
    pub(crate) cost: f64, pub(crate) probability: f64, pub(crate) time: i64, pub(crate) start: i64, pub(crate) end: i64,
    pub(crate) completed: Option<i64>, pub(crate) completed_by: Option<i64>, pub(crate) status: i32,
}
impl Job {
    fn from_row(r: &Row) -> Self {
        Self { id: r.get("job_id"), installer: r.get("installer_id"), owner: r.get("owner_id"), activity: r.get("activity_id"),
            blueprint_id: r.get("blueprint_id"), blueprint_type: r.get("blueprint_type_id"), blueprint_copy: r.get("blueprint_copy"),
            facility: r.get("facility_id"), system: r.get("solar_system_id"), runs: r.get("runs"), licensed_runs: r.get("licensed_runs"),
            successful_runs: r.get("successful_runs"), product_type: r.get("product_type_id"), optional_type: r.get("optional_type_id"),
            cost: r.get("cost"), probability: r.get("probability"), time: r.get("time_seconds"), start: r.get("start_date"),
            end: r.get("end_date"), completed: r.get("completed_date"), completed_by: r.get("completed_by"), status: r.get("status") }
    }
}
struct CountRow { activity_id: i32, count: i64 }
impl CountRow { fn from_row(r: &Row) -> Self { Self { activity_id: r.get("activity_id"), count: r.get("count") } } }
struct PriceRow { type_id: i32, adjusted: f64 }
impl PriceRow { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), adjusted: r.get("adjusted") } } }
struct AvailableRow { type_id: i32, quantity: i64 }
impl AvailableRow { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), quantity: r.get("quantity") } } }
struct BusyRow { exists: bool }
impl BusyRow { fn from_row(r: &Row) -> Self { Self { exists: r.get("exists") } } }
struct LocationRow { location_id: i64 }
impl LocationRow { fn from_row(r: &Row) -> Self { Self { location_id: r.get("location_id") } } }
struct ItemIdRow { item_id: i64 }
impl ItemIdRow { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }
struct JobIdRow { job_id: i64 }
impl JobIdRow { fn from_row(r: &Row) -> Self { Self { job_id: r.get("job_id") } } }

pub(crate) async fn blueprint(ex: &(impl Executor + ?Sized), item_id: i64) -> Result<Option<Blueprint>, db::Error> {
    Ok(ex.query_opt(BLUEPRINT_SQL, &[&item_id, &BLUEPRINT_INFINITE_RUNS]).await?.as_ref().map(Blueprint::from_row))
}
pub(crate) async fn set_blueprint(ex: &(impl Executor + ?Sized), item_id: i64, me: i32, te: i32, runs: i32) -> Result<(), db::Error> {
    ex.execute(SET_BLUEPRINT_SQL, &[&item_id, &me, &te, &runs]).await?; Ok(())
}
pub(crate) async fn job(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Job>, db::Error> {
    Ok(ex.query_opt(&format!("{JOB_SELECT} WHERE job_id = $1"), &[&id]).await?.as_ref().map(Job::from_row))
}
pub(crate) async fn jobs_by_owner(ex: &(impl Executor + ?Sized), owner: i64, completed: bool, installed_status: i32, cutoff: i64) -> Result<Vec<Job>, db::Error> {
    let rows = if completed {
        ex.query(&format!("{JOB_SELECT} WHERE owner_id = $1 AND (status = $2 OR completed_date >= $3) ORDER BY job_id"), &[&owner, &installed_status, &cutoff]).await?
    } else {
        ex.query(&format!("{JOB_SELECT} WHERE owner_id = $1 AND status = $2 ORDER BY job_id"), &[&owner, &installed_status]).await?
    };
    Ok(rows.iter().map(Job::from_row).collect())
}
pub(crate) async fn job_counts(ex: &(impl Executor + ?Sized), installer: i64, installed_status: i32) -> Result<HashMap<i32, i64>, db::Error> {
    Ok(ex.query(COUNTS_SQL, &[&installer, &installed_status]).await?.iter().map(|r| { let r = CountRow::from_row(r); (r.activity_id, r.count) }).collect())
}
pub(crate) async fn prices(ex: &(impl Executor + ?Sized), ids: &[i32]) -> Result<HashMap<i32, f64>, db::Error> {
    Ok(ex.query(PRICES_SQL, &[&ids]).await?.iter().map(|r| { let r = PriceRow::from_row(r); (r.type_id, r.adjusted) }).collect())
}
pub(crate) async fn available(ex: &(impl Executor + ?Sized), owner: i64, station: i64, flag: i32) -> Result<HashMap<i32, i64>, db::Error> {
    Ok(ex.query(AVAILABLE_SQL, &[&owner, &station, &flag]).await?.iter().map(|r| { let r = AvailableRow::from_row(r); (r.type_id, r.quantity) }).collect())
}
pub(crate) async fn blueprint_busy(ex: &(impl Executor + ?Sized), item_id: i64, installed_status: i32) -> Result<bool, db::Error> {
    Ok(BusyRow::from_row(&ex.query_one(BUSY_SQL, &[&item_id, &installed_status]).await?).exists)
}
pub(crate) async fn item_location(ex: &(impl Executor + ?Sized), item_id: i64) -> Result<i64, db::Error> {
    Ok(LocationRow::from_row(&ex.query_one(LOCATION_SQL, &[&item_id]).await?).location_id)
}
pub(crate) async fn make_singleton(ex: &(impl Executor + ?Sized), item_id: i64) -> Result<(), db::Error> {
    ex.execute(MAKE_SINGLETON_SQL, &[&item_id]).await?; Ok(())
}
pub(crate) async fn insert_job(ex: &(impl Executor + ?Sized), job: &Job) -> Result<i64, db::Error> {
    Ok(JobIdRow::from_row(&ex.query_one(INSERT_JOB_SQL, &[&job.installer, &job.owner, &job.activity, &job.blueprint_id, &job.blueprint_type, &job.blueprint_copy, &job.facility, &job.system, &job.runs, &job.licensed_runs, &job.product_type, &job.optional_type, &job.cost, &job.probability, &job.time, &job.start, &job.end, &job.status]).await?).job_id)
}
pub(crate) async fn delete_blueprint_item(ex: &(impl Executor + ?Sized), item_id: i64) -> Result<(), db::Error> {
    crate::dao::items::inventory::delete(ex, item_id).await?;
    ex.execute(DELETE_BLUEPRINT_SQL, &[&item_id]).await?; Ok(())
}
pub(crate) async fn insert_copy(ex: &(impl Executor + ?Sized), type_id: i32, owner: i64, station: i64, flag: i32, quantity: i64) -> Result<i64, db::Error> {
    Ok(ItemIdRow::from_row(&ex.query_one(INSERT_COPY_SQL, &[&type_id, &owner, &station, &flag, &quantity]).await?).item_id)
}
pub(crate) async fn deliver_job(ex: &(impl Executor + ?Sized), job: &Job, installed_status: i32) -> Result<u64, db::Error> {
    ex.execute(DELIVER_SQL, &[&job.id, &job.status, &job.successful_runs, &job.completed, &job.completed_by, &installed_status]).await
}
pub(crate) async fn cancel_job(ex: &(impl Executor + ?Sized), job: &Job, completed_by: i64, installed_status: i32) -> Result<u64, db::Error> {
    ex.execute(CANCEL_SQL, &[&job.id, &job.status, &job.completed, &completed_by, &installed_status]).await
}
