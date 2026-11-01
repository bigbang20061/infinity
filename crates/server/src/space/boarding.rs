//! Owned in-space ship boarding invoked by `eve/client/script/ui/services/menuSvcExtras/menuFunctions.py`
//! and `eve/client/script/dogma/clientDogmaLocation.py`. Type categories come from
//! the SDE; committed ship and condition data are prepared by the caller, or
//! reloaded after an actor stops. Authorization samples motion once and does not
//! reserve either hull or perform database work.
use super::*;
use crate::consts::CATEGORY_SHIP;
use crate::dao::items::space_ships::FreeShip;
use crate::services::{crimewatch, ship::balls::{self, PreparedShip}, CallError};

#[derive(Clone)]
pub(crate) struct BoardSnapshot {
    pub old: Ball,
    pub target: Ball,
    pub old_generation: u64,
    pub target_generation: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) enum BoardRefusal { StalePilot, InvalidTarget, Occupied, Unsafe, OutOfRange }

impl System {
    /// Sample and authorize without changing actor state.
    pub(crate) fn authorize_board(&self, character: i64, old: i64, target: i64, max_gap: f64) -> Result<BoardSnapshot, BoardRefusal> {
        let old_ball = self.pilot_ball(character).filter(|b| b.id == old && b.char_id == Some(character) && b.owner_id == character)
            .ok_or(BoardRefusal::StalePilot)?;
        let old_generation = self.persistent_generation(old).ok_or(BoardRefusal::StalePilot)?;
        let is_ship = |b: &Ball| self.sde().and_then(|sde| sde.types.get(b.type_id))
            .is_some_and(|ty| ty.category_id == Some(CATEGORY_SHIP));
        if !is_ship(old_ball) { return Err(BoardRefusal::StalePilot); }
        if old == target { return Err(BoardRefusal::InvalidTarget); }
        let target_ball = self.ball(target).filter(|b| b.owner_id == character && is_ship(b))
            .ok_or(BoardRefusal::InvalidTarget)?;
        let target_generation = self.persistent_generation(target).ok_or(BoardRefusal::InvalidTarget)?;
        if target_ball.char_id.is_some() || target_ball.interactive { return Err(BoardRefusal::Occupied); }
        if old_ball.mode == MODE_WARP || target_ball.mode == MODE_WARP || old_ball.cloak != 0 || target_ball.cloak != 0 {
            return Err(BoardRefusal::Unsafe);
        }
        let old = self.sampled_ball(old).ok_or(BoardRefusal::StalePilot)?;
        let target = self.sampled_ball(target).ok_or(BoardRefusal::InvalidTarget)?;
        if !max_gap.is_finite() || max_gap < 0.0 || !old.radius.is_finite() || old.radius < 0.0
            || !target.radius.is_finite() || target.radius < 0.0
            || !old.pos.iter().chain(target.pos.iter()).chain(old.vel.iter()).chain(target.vel.iter()).all(|v| v.is_finite()) {
            return Err(BoardRefusal::Unsafe);
        }
        let distance = dist(old.pos, target.pos);
        if !distance.is_finite() || (distance - f64::from(old.radius) - f64::from(target.radius)).max(0.0) > max_gap {
            return Err(BoardRefusal::OutOfRange);
        }
        Ok(BoardSnapshot { old, target, old_generation, target_generation })
    }

    /// Publish committed seeds even if membership changed after authorization.
    pub(crate) fn apply_committed_board(&mut self, client_id: i64, character: i64, snapshot: BoardSnapshot, old: PreparedShip, target: PreparedShip) {
        let old_id = snapshot.old.id;
        let target_id = snapshot.target.id;
        if self.persistent_generation(old_id) != Some(snapshot.old_generation)
            || self.persistent_generation(target_id) != Some(snapshot.target_generation)
            || self.pilot_ball(character).is_none_or(|b| b.id != old_id) {
            tracing::warn!(system = self.id, character, old_id, target_id, "committed boarding membership changed before publication");
        }
        for id in [old_id, target_id] {
            if let Some(ball) = self.take(id) {
                if ball.cloak == 0 {
                    let update = event(self.current_time, "RemoveBalls", vec![Value::list([Value::Int(id)])]);
                    self.actions.push_back((self.current_time, Box::new(move |s, _| s.deliver(None, vec![update]))));
                }
            }
        }
        let mut old = old.install();
        let mut target = target.install();
        old.pos = snapshot.old.pos;
        target.pos = snapshot.target.pos;
        for ball in [&mut old, &mut target] {
            ball.stop();
            ball.vel = [0.0; 3];
            ball.speed_fraction = 0.0;
        }
        if let Some((ego, _)) = self.observers.get_mut(&client_id) { *ego = target_id; }
        self.add_persistent_ship(old);
        self.add_ship(target);
    }
}

impl World {
    /// Retry a stopped actor without losing the owned committed seeds.
    pub(crate) async fn apply_committed_board(&self, system: i64, client_id: i64, character: i64, snapshot: BoardSnapshot, old: PreparedShip, target: PreparedShip) -> anyhow::Result<()> {
        let descriptor = |ball: &Ball, pos| FreeShip {
            item_id: ball.id, type_id: ball.type_id, owner_id: ball.owner_id,
            corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos,
        };
        let old_ship = descriptor(&old.ball, snapshot.old.pos);
        let target_ship = descriptor(&target.ball, snapshot.target.pos);
        let payload = Arc::new(Mutex::new(Some((old, target))));
        loop {
            self.system(system).await?;
            let shared = payload.clone();
            let committed = snapshot.clone();
            let result = self.with(system, move |s| {
                let (old, target) = shared.lock().unwrap().take().expect("boarding payload already consumed");
                s.apply_committed_board(client_id, character, committed, old, target);
            }).await;
            if result.is_some() { return Ok(()); }
            let consumed = payload.lock().unwrap().is_none();
            if consumed {
                let to_error = |error| match error {
                    CallError::Internal(error) => error,
                    CallError::User { msg, .. } => anyhow::anyhow!(msg),
                };
                let security = crimewatch::security_status(&self.db, character).await.map_err(to_error)?;
                let old = balls::unpiloted(&self.db, self.sde.clone(), &old_ship).await.map_err(to_error)?;
                let target = balls::piloted(&self.db, self.sde.clone(), &target_ship, character, security).await.map_err(to_error)?;
                *payload.lock().unwrap() = Some((old, target));
            }
        }
    }
}

#[cfg(test)]
#[path = "boarding/tests.rs"]
mod tests;
