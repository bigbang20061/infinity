//! Ship location and home station persistence from game.characters and SDE stations.
//! Ship services retain client behavior, transactions and notifications.
//! Station row decoding is shared with `characters::clones`.
use crate::db::{self, executor::Executor};
use super::clones::Station;
pub(crate) async fn undock(ex:&(impl Executor+?Sized), me:i64, system:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET station_id = NULL, solar_system_id = $2 WHERE character_id = $1", &[&me,&system]).await }
pub(crate) async fn dock(ex:&(impl Executor+?Sized), me:i64, station:i64, system:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET station_id = $2, solar_system_id = $3 WHERE character_id = $1", &[&me,&station,&system]).await }
pub(crate) async fn location(ex:&(impl Executor+?Sized), me:i64, system:i64, constellation:i64, region:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET solar_system_id = $2, constellation_id = $3, region_id = $4 WHERE character_id = $1", &[&me,&system,&constellation,&region]).await }
pub(crate) async fn home(ex:&(impl Executor+?Sized), me:i64)->Result<Station,db::Error> { Ok(Station::from_row(&ex.query_one("SELECT s.station_id::bigint AS station_id, s.solar_system_id::bigint AS solar_system_id, s.constellation_id::bigint AS constellation_id, s.region_id::bigint AS region_id
                     FROM game.characters ch JOIN sde.sta_stations s ON s.station_id = COALESCE(ch.home_station_id, ch.station_id)
                     WHERE ch.character_id = $1", &[&me]).await?)) }
pub(crate) async fn set_home(ex:&(impl Executor+?Sized), me:i64, capsule:i64, station:i64, system:i64, constellation:i64, region:i64)->Result<u64,db::Error> { ex.execute("UPDATE game.characters SET ship_id = $2, station_id = $3, solar_system_id = $4, constellation_id = $5, region_id = $6 WHERE character_id = $1", &[&me,&capsule,&station,&system,&constellation,&region]).await }
