use super::training::plan_skills;

#[test]
fn completed_and_banked_skills_are_aggregated_in_type_order() {
    let plan = plan_skills(&[(9, 2, 100), (3, 1, 50), (9, 1, 150)], Some((9, 175)));
    assert_eq!(plan.into_iter().collect::<Vec<_>>(), vec![(3, (Some(1), 50)), (9, (Some(2), 175))]);
}

#[test]
fn banking_only_remains_update_only() {
    let plan = plan_skills(&[(9, 1, 50)], Some((3, 100)));
    assert_eq!(plan.into_iter().collect::<Vec<_>>(), vec![(3, (None, 100)), (9, (Some(1), 50))]);
}
