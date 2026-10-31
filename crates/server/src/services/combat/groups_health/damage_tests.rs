use super::set_damage_layers;

#[test]
fn selected_layers_change_without_healing_other_damage() {
    let max = [100.0, 200.0, 300.0];
    let now = [80.0, 100.0, 150.0];
    assert_eq!(set_damage_layers(now, max, [true, false, false], 0.25), [75.0, 100.0, 150.0]);
    assert_eq!(set_damage_layers(now, max, [true; 3], 0.5), [50.0, 100.0, 150.0]);
    assert_eq!(set_damage_layers(now, max, [false, true, false], 1.0), [80.0, 0.0, 150.0]);
}
