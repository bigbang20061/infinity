use super::*;
use crate::space::System;

#[test]
fn missile_lifecycle_preserves_exploration_site() {
    let mut system = System::new(0, Arc::default(), 12_345);
    let site = Ball {
        id: 9_500_000_000_000, type_id: 1, owner_id: 1, corp_id: 1, alliance_id: -1,
        char_id: None, signature_radius: None, mode: crate::space::MODE_STOP, radius: 1.0, mass: 1.0,
        max_vel: 1.0, agility: 1.0, speed_fraction: 0.0,
        pos: [1.0, 2.0, 3.0], vel: [0.0; 3], goto: [0.0; 3],
        old_pos: [1.0, 2.0, 3.0], old_vel: [0.0; 3], segment_acceleration: None,
        follow_id: 0, follow_range: 0.0, free: true, global: false, interactive: false,
        massive: true, warp: crate::space::WarpState::default(), warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0,
        cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new(),
    };
    let (site_id, site_type, site_pos) = (site.id, site.type_id, site.pos);
    let missile_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    let missile = Ball {
        id: missile_id, type_id: 2, mode: crate::space::MODE_STOP, pos: [4.0, 5.0, 6.0],
        ..site.clone()
    };
    system.insert(site);
    system.launch_missile(missile, site_id, site_id, 500, move |s| { s.destroy_ball(missile_id); });

    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
    let retained_missile = system.ball(missile_id).unwrap();
    assert_eq!((retained_missile.id, retained_missile.type_id, retained_missile.pos), (missile_id, 2, [4.0, 5.0, 6.0]));
    let second_id = NEXT_MISSILE_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(missile_id, second_id);
    assert_eq!(second_id, missile_id + 1);

    system.tick(13_000);
    system.tick(13_500);
    assert!(!system.contains(missile_id));
    let retained_site = system.ball(site_id).unwrap();
    assert_eq!((retained_site.id, retained_site.type_id, retained_site.pos), (site_id, site_type, site_pos));
}


#[test]
fn hit_quality_bands() {
    assert_eq!(hit_quality(0.0), 0);
    assert_eq!(hit_quality(0.5), 1);
    assert_eq!(hit_quality(1.1), 4);
    assert_eq!(hit_quality(3.0), 6);
}

