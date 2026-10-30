use super::*;

#[test]
fn checkpoints_and_goals_build() {
    let gw = Gateway { cdn_url: "http://127.0.0.1".into(), ..Default::default() };
    for (req, content) in [
        ("eve_public.achievement.definition.api.cdn.GetLatestRequest", "achievements"),
        ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", "titles"),
        ("eve_public.space.api.cdn.GetUpdateRequest", "maptags"),
    ] {
        let m = checkpoint(&gw, req, content).unwrap();
        assert!(format!("{m}").contains(&format!("http://127.0.0.1/content/{content}")));
    }
    assert_eq!(career_goal_definitions().unwrap().get_field_by_name("goals").unwrap().as_list().unwrap().len(), 4);
    career_goal_states().unwrap();
    stage_thresholds("eve_public.pirate.corruption.api.GetStageThresholdsRequest").unwrap();
}

#[test]
fn defaults_say_absent_and_echo_ids() {
    let caller = Caller { user_id: Some(1), character_id: Some(90000001) };
    let mut m = response_for("eve_public.matrix.api.GetEquippedRequest").unwrap();
    let ids = super::super::known_ids(&m, &caller);
    super::super::fill_defaults(&mut m, 4, &ids);
    assert!(format!("{m}").contains("no_equipped:true"), "{m}");

    let mut m = response_for("eve_public.pirate.corruption.api.GetSystemInfoRequest").unwrap();
    super::super::fill_defaults(&mut m, 4, &Default::default());
    assert!(format!("{m}").contains("denominator:1"), "{m}");

    let mut req = DynamicMessage::new(message("eve_public.cosmetic.ship.api.GetRequest").unwrap());
    let ship = identifier(&req.descriptor(), "ship", PV::U64(1000000000001)).unwrap();
    set(&mut req, "ship", PV::Message(ship)).unwrap();
    let mut m = response_for("eve_public.cosmetic.ship.api.GetRequest").unwrap();
    super::super::fill_defaults(&mut m, 4, &super::super::known_ids(&req, &caller));
    let s = format!("{m}");
    assert!(s.contains("1000000000001") && s.contains("90000001"), "{s}");
}
