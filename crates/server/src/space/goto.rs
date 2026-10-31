//! Native GOTO initialization and thrust for transient destiny balls.
//! Mirrors carbonengine destiny `Ballpark.cpp` GotoPoint, GotoDirection and GotoThrust,
//! as consumed by `eve/client/script/remote/michelle.py`; state is in memory.
//! Warp and missile behavior are implemented separately.

use super::{add, norm, scale, sub, Ball, V3, DESTINY_FRICTION, GOTO_DIRECTION_REACH, MODE_GOTO};

impl Ball {
    /// Set an absolute destination, rejecting non-finite coordinates.
    pub fn goto_point(&mut self, point: V3) -> bool {
        if !point.iter().all(|v| v.is_finite()) { return false; }
        self.stop();
        self.goto = point;
        if self.speed_fraction == 0.0 { self.speed_fraction = 1.0; }
        self.mode = MODE_GOTO;
        true
    }

    /// Project a raw direction to the native distant GOTO point.
    pub fn goto_direction(&mut self, dir: V3) -> bool {
        if !dir.iter().all(|v| v.is_finite()) { return false; }
        self.goto_point(add(self.pos, scale(norm(dir), GOTO_DIRECTION_REACH)))
    }

    pub(super) fn goto_thrust(&self, target: V3, missile: bool) -> V3 {
        let a = sub(target, self.pos);
        let length2 = a[0] * a[0] + a[1] * a[1] + a[2] * a[2];
        let distance = self.speed_fraction as f64 * self.max_vel as f64;
        let max_thrust = DESTINY_FRICTION * self.speed_fraction as f64 * self.max_vel as f64
            / (self.mass * self.agility as f64);
        let thrust = if !missile && length2 < distance * distance {
            let coefficient = length2 / (distance * distance);
            max_thrust * coefficient * coefficient
        } else { max_thrust };
        scale(norm(a), thrust)
    }

    pub(super) fn evolve_goto(&self) -> V3 { self.goto_thrust(self.goto, false) }
}
