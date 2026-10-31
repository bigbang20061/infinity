//! Native FOLLOW initialization and pursuit for transient destiny balls.
//! Mirrors carbonengine destiny `Ballpark.cpp` FollowBall and EvolveFollow,
//! as consumed by `eve/client/script/remote/michelle.py`; state is in memory.
//! Orbit and missile interception are implemented separately.

use super::{add, len, scale, sub, Ball, TargetState, V3, MODE_FOLLOW};

impl Ball {
    /// Start following a visible, distinct target with a finite range.
    pub fn follow_ball(&mut self, target: &Ball, range: f32) -> bool {
        if !range.is_finite() || target.id == self.id || target.cloak != 0 { return false; }
        self.stop();
        self.follow_id = target.id;
        self.follow_range = range;
        self.mode = MODE_FOLLOW;
        true
    }

    pub(super) fn evolve_follow(&mut self, target: &TargetState) -> V3 {
        let delta = sub(self.pos, target.pos);
        let distance = len(delta);
        let radius = self.follow_range as f64 + self.radius as f64 + target.radius as f64;
        let offset = if distance == 0.0 { scale([1.0, 0.0, 0.0], radius) }
            else { scale(scale(delta, radius), 1.0 / distance) };
        self.goto = add(target.pos, offset);
        self.goto_thrust(self.goto, false)
    }
}
