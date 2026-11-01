use super::*;
use crate::services::effects::{EffectKind, JammingType};

#[test]
fn expiration_invalidates_the_receiving_snapshot() {
    let mut system = System::default();
    let ball = Ball {
        id: 1, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: MODE_STOP,
        radius: 1.0, mass: 1.0, max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [0.0; 3], vel: [0.0; 3], goto: [0.0; 3], follow_id: 0,
        old_pos: [0.0; 3], old_vel: [0.0; 3], segment_acceleration: None,
        follow_range: 0.0, free: true, global: false, massive: false, interactive: false,
        warp: WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
        cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    system.insert(ball.clone());
    system.insert(Ball { id: 2, ..ball });
    system.upsert_projection(1, Projection {
        source_ship: 2, source_module: 3, source_type: 1, effect_id: 1,
        kind: EffectKind::Projected { jamming: JammingType::Webify }, until_ms: 0,
        values: vec![], charge_values: vec![], charge_type: None,
    });
    let revision = system.projection_revision(1);
    assert_eq!(system.projections_on(1).len(), 1);
    system.expire_projections();
    assert!(system.projections_on(1).is_empty());
    assert!(system.projection_revision(1) > revision);
}
