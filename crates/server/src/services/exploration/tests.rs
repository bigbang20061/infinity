use super::*;

fn site(pos: V3, ease: f64) -> Site {
    new_site(1, pos, Kind::Signature, STRENGTH_ALL, ease, &mut HashSet::new())
}

#[test]
fn pinpoint_formation_resolves_a_site() {
    let s = site([0.0; 3], 0.6);
    let around = |r: f64| (0..4).map(|i| ([r * 0.4 * (i as f64 - 1.5), 0.0, 0.0], r, 40.0 * 1.1025)).collect::<Vec<_>>();
    assert_eq!(certainty(&s, &around(0.25 * AU)).0, 1.0);
    let wide = certainty(&s, &around(8.0 * AU)).0;
    assert!(wide > 0.0 && wide < 0.25, "{wide}");
    // Three probes never pin it down.
    assert!(certainty(&s, &around(0.25 * AU)[..3]).0 < 0.75);
}

#[test]
fn target_ids_look_like_retail() {
    let id = target_id(&HashSet::new());
    assert_eq!(id.len(), 7);
    assert_eq!(&id[3..4], "-");
}
