//! Ship balls for selected and persistent unpiloted hulls. Destiny consumes the
//! resulting state through `eve/client/script/remote/michelle.py`. Type data
//! comes from the SDE and fittings and condition from game item dogma rows.
//! Unpiloted hulls have no pilot or security status; their condition is installed
//! only when their prepared ball is published.

use std::sync::Arc;
use crate::client::Client;
use crate::consts::dogma::{ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_SIGNATURE_RADIUS};
use crate::dao::items::space_ships::FreeShip;
use crate::db::Db;
use crate::sde::Sde;
use crate::services::{combat, crimewatch, db_err, fit::{Fit, Key}, CallError};
use crate::space::{self, now_ms, Ball, MODE_STOP};
use marshal::Value;

const ATTR_WARP_SPEED_MULTIPLIER: i32 = 600;
const ATTR_BASE_WARP_SPEED: i32 = 1281;
/// Server policy: ship radius in metres when the SDE has no radius.
const DEFAULT_SHIP_RADIUS: f64 = 50.0;

/// Collision radius used for both ship placement and its destiny ball.
pub(crate) fn radius(sde: &Sde, type_id: i32) -> Result<f32, CallError> {
    Ok(super::required(sde.types.get(type_id), "ship type")?.radius.unwrap_or(DEFAULT_SHIP_RADIUS) as f32)
}

pub(crate) struct PreparedShip {
    pub ball: Ball,
    condition: combat::ConditionSeed,
}

impl PreparedShip {
    pub(crate) fn install(self) -> Ball {
        self.condition.install(self.ball.id);
        self.ball
    }
}

pub(super) async fn selected(c: &Client, ship_id: i64, pos: [f64; 3]) -> Result<Ball, CallError> {
    let type_id = super::required(crate::dao::items::inventory::type_id(&c.db, ship_id).await.map_err(db_err)?, "ship item")?;
    let char_id = c.session.int("charid").unwrap_or(0);
    let fit = if let Some(system) = c.session.int("solarsystemid") {
        Fit::load_in_space(&c.db, &c.world, system, char_id, ship_id).await?
    } else {
        Fit::load(&c.db, c.sde.clone(), char_id, ship_id).await?
    };
    let security = crimewatch::security_status(&c.db, char_id).await?;
    let health = combat::load_condition_with_fit(&c.db, ship_id, &fit).await?;
    build(&c.sde, ship_id, type_id, char_id, c.session.int("corpid").unwrap_or(0),
        c.session.int("allianceid").unwrap_or(-1), Some(char_id), pos, &fit, health, Some(security))
}

/// Prepare an unpiloted persistent ship without publishing its condition.
pub(crate) async fn unpiloted(db: &Db, sde: Arc<Sde>, ship: &FreeShip) -> Result<PreparedShip, CallError> {
    let fit = Fit::load_unpiloted(db, sde.clone(), ship.item_id).await?;
    let condition = combat::prepare_condition_with_fit(db, ship.item_id, &fit).await?;
    let ball = build(&sde, ship.item_id, ship.type_id, ship.owner_id, ship.corporation_id,
        ship.alliance_id, None, ship.pos, &fit, condition.fractions(), None)?;
    Ok(PreparedShip { ball, condition })
}

fn build(sde: &Sde, id: i64, type_id: i32, owner_id: i64, corp_id: i64, alliance_id: i64,
    char_id: Option<i64>, pos: [f64; 3], fit: &Fit, health: [f64; 3], security: Option<f64>) -> Result<Ball, CallError> {
    let r = super::required(sde.types.get(type_id), "ship type")?;
    let fitted = |attr: i32, base: f64| match fit.value(Key::Ship, attr) {
        v if v > 0.0 => v,
        _ => base,
    };
    let mass = fitted(ATTR_MASS, r.attribute(ATTR_MASS).or(r.mass).unwrap_or(1_000_000.0));
    let warp_mult = r.attribute(ATTR_WARP_SPEED_MULTIPLIER).unwrap_or(1.0);
    let base_warp = r.attribute(ATTR_BASE_WARP_SPEED).unwrap_or(3.0);
    let radius = radius(sde, type_id)?;
    Ok(Ball {
        id, type_id, owner_id, corp_id, alliance_id, char_id,
        mode: MODE_STOP,
        radius,
        signature_radius: Some(fitted(ATTR_SIGNATURE_RADIUS, radius as f64)),
        mass,
        max_vel: fitted(ATTR_MAX_VELOCITY, r.attribute(ATTR_MAX_VELOCITY).unwrap_or(200.0)) as f32,
        agility: fitted(ATTR_AGILITY, r.attribute(ATTR_AGILITY).unwrap_or(3.0)) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        interactive: char_id.is_some(),
        warp: space::WarpState::default(),
        warp_factor: ((warp_mult * base_warp * 1000.0).round() as i64).max(500),
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health,
        launcher: 0,
        slim: security.map(|v| vec![("securityStatus", Value::Float(v))]).unwrap_or_default(),
    })
}

#[cfg(test)] mod tests;
