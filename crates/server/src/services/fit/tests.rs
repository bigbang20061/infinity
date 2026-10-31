use super::apply;

#[test]
fn operations_and_stacking() {
    // 3 x 5% damage skill bonus (PostPercent, unpenalized) on 2.0
    assert!((apply(2.0, &[(6, 15.0, false)]) - 2.3).abs() < 1e-9);
    // Two 10% penalized damage mods: 1.1 * (1 + 0.1 * e^-(1/2.67)^2)
    let two = apply(1.0, &[(4, 1.1, true), (4, 1.1, true)]);
    assert!((two - 1.1 * (1.0 + 0.1 * (-(1.0f64 / 2.67).powi(2)).exp())).abs() < 1e-9);
    assert_eq!(apply(5.0, &[(2, 3.0, false), (7, 1.0, false)]), 1.0);
}

#[test]
fn unknown_type_uses_attribute_default() {
    let meta = super::Attribute { attribute_id: 1, default_value: Some(7.5), stackable: None };
    assert_eq!(super::base_attribute(None, Some(&meta), 1), 7.5);
    assert_eq!(super::base_attribute(None, None, 1), 0.0);
    let null_default = super::Attribute { default_value: None, ..meta };
    assert_eq!(super::base_attribute(None, Some(&null_default), 1), 0.0);
}

#[test]
fn node_override_precedence_and_modifier() {
    use super::{Node, ATTR_SKILL_LEVEL};
    let mut node = Node { type_id: 1, data_position: None, in_ship: false, online: true, active: false, level: None, overrides: vec![(1, 10.0), (ATTR_SKILL_LEVEL, 99.0)] };
    let meta = super::Attribute { attribute_id: 1, default_value: Some(7.5), stackable: None };
    assert_eq!(node.base(None, Some(&meta), 1), 10.0);
    assert_eq!(node.base(None, None, 2), 0.0);
    assert_eq!(apply(node.base(None, Some(&meta), 1), &[(6, 25.0, false)]), 12.5);
    node.level = Some(4);
    assert_eq!(node.base(None, None, ATTR_SKILL_LEVEL), 4.0);
    node.overrides.clear();
    assert_eq!(node.base(None, Some(&meta), 1), 7.5);
}

