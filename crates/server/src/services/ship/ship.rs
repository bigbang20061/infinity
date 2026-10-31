//! The ship client service, mirroring the client ship service; uses SDE celestials and game locations. Space movement is handled by beyonce.
use super::*;

async fn undock(c: &mut Client) -> CallResult {
    let (Some(station), Some(system), Some(ship)) = (c.session.int("stationid"), c.session.int("solarsystemid2"), c.session.int("shipid")) else {
        return Err(CallError::User { msg: messages::MSG_CANT_UNDOCK_NOT_IN_STATION.into(), dict: vec![] });
    };
    let st = required(c.sde.map.celestial(station), "station celestial")?;
    let spos: [f64; 3] = [required(st.x, "station x")?, required(st.y, "station y")?, required(st.z, "station z")?];
    // Leave along a fixed per-station heading.
    let a = (station % 360) as f64 * std::f64::consts::PI / 180.0;
    let dir = [a.cos(), 0.0, a.sin()];
    let dist = f64::from(c.sde.celestial_radius(st)) + 2500.0;
    let pos = [spos[0] + dir[0] * dist, spos[1], spos[2] + dir[2] * dist];
    let mut ball = ship_ball(c, ship, pos).await?;
    ball.vel = [dir[0] * ball.max_vel as f64, 0.0, dir[2] * ball.max_vel as f64];
    let _lock = crate::services::market::lock().await;
    crate::services::trade::on_leave_locked(c).await?;
    item_dao::undock(&c.db, ship, system, pos).await.map_err(db_err)?;
    character_dao::undock(&c.db, c.session.int("charid").unwrap_or(0), system).await.map_err(db_err)?;
    c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    info!(user = c.login.user_id, "ship {ship} undocked from {station} into {system}");
    c.change_session(vec![("stationid", Value::None), ("solarsystemid", Value::Int(system))]);
    Ok(Value::None)
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "Undock" => Some(undock(c).await),
        _ => None,
    }
}
