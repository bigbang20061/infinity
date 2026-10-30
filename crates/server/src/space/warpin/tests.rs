use super::*;

#[test]
fn matches_python_random() {
    // random.Random(seed).random(); integer seeding is the same in Python 2.7 and 3.
    assert_eq!(PyRandom::new(42).random(), 0.6394267984578837);
    assert_eq!(PyRandom::new(0).random(), 0.8444218515250481);
    assert_eq!(PyRandom::new(40009077).random(), 0.34917398642449415);
    assert_eq!(PyRandom::new((1 << 40) + 5).random(), 0.5043802970418443);
}

#[test]
fn planet_matches_client() {
    // eveCfg.GetPlanetWarpInPoint for two Jita planets.
    let close = |a: V3, b: V3| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3);
    let p = warp_in_point(40009077, 2, 7, [-35639949630.0, -6225947509.0, 20551935633.0], 2060000.0).unwrap();
    assert!(close(p, [-35645834172.27962, -6226169210.646901, 20545491225.999252]), "{p:?}");
    let p = warp_in_point(40009098, 2, 7, [-639929607985.0, -111789387758.0, -1118379774141.0], 57970000.0).unwrap();
    assert!(close(p, [-639868036420.8906, -111797277339.89847, -1118442589385.8162]), "{p:?}");
}
