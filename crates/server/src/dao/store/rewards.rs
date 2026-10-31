//! Seasonal login reward and daily gift persistence for the store client.
//! Uses game.login_reward_claims and game.store_purchases; campaign content comes from the SDE.
use crate::db::{executor::Executor, Error};
const REWARD_DAY_SQL: &str = "(now() AT TIME ZONE 'UTC' - $3::text::interval)::date";
struct Claimed { claimed: bool }
impl Claimed { fn from_row(r:&tokio_postgres::Row)->Self { Self { claimed:r.get("claimed") } } }
struct Claims { count:i64, today:bool }
impl Claims { fn from_row(r:&tokio_postgres::Row)->Self { Self { count:r.get("count"), today:r.get("today") } } }
pub(crate) struct History { pub ordinal:i32, pub clone_grade:i32 }
impl History { fn from_row(r:&tokio_postgres::Row)->Self { Self { ordinal:r.get("ordinal"), clone_grade:r.get("clone_grade") } } }
pub(crate) async fn daily_gift(ex:&impl Executor,user:i64,offer:i32)->Result<bool,Error> {
    let turnover = &crate::config::game().store.daily_turnover_utc;
    Ok(Claimed::from_row(&ex.query_one(&format!("SELECT EXISTS (SELECT 1 FROM game.store_purchases WHERE user_id = $1 AND offer_id = $2
                 AND (purchased_at AT TIME ZONE 'UTC' - $3::text::interval)::date = {REWARD_DAY_SQL}) AS claimed"),&[&user,&offer,turnover]).await?).claimed)
}
pub(crate) async fn claims(ex:&impl Executor,user:i64,campaign:i32)->Result<(i64,bool),Error> {
    let turnover = &crate::config::game().store.daily_turnover_utc;
    let r=Claims::from_row(&ex.query_one(&format!("SELECT count(*), count(*) FILTER (WHERE day = {REWARD_DAY_SQL}) > 0 AS today FROM game.login_reward_claims WHERE user_id = $1 AND campaign_id = $2"),&[&user,&campaign,turnover]).await?); Ok((r.count,r.today))
}
pub(crate) async fn history(ex:&impl Executor,user:i64,campaign:i32)->Result<Vec<History>,Error> { Ok(ex.query("SELECT (day - date '0001-01-01') + 1 AS ordinal, clone_grade FROM game.login_reward_claims WHERE user_id = $1 AND campaign_id = $2",&[&user,&campaign]).await?.iter().map(History::from_row).collect()) }
pub(crate) async fn claim(ex:&impl Executor,user:i64,campaign:i32)->Result<u64,Error> {
    let turnover = &crate::config::game().store.daily_turnover_utc;
    ex.execute(&format!("INSERT INTO game.login_reward_claims (user_id, campaign_id, day, clone_grade) VALUES ($1, $2, {REWARD_DAY_SQL}, 1)"),&[&user,&campaign,turnover]).await
}
pub(crate) async fn today_ordinal(ex:&impl Executor)->Result<i32,Error> {
    let turnover = &crate::config::game().store.daily_turnover_utc;
    Ok(ex.query_one("SELECT (((now() AT TIME ZONE 'UTC' - $1::text::interval)::date - date '0001-01-01') + 1) AS ordinal", &[turnover]).await?.get("ordinal"))
}
