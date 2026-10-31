use super::*;

fn effect(name: &str) -> Effect {
    Effect {
        effect_id: 1, name: Some(name.into()), guid: None, effect_category: None,
        duration_attribute_id: None, is_offensive: false, is_assistance: false,
        disallow_auto_repeat: false, electronic_chance: false, propulsion_chance: false,
        discharge_attribute_id: None, range_attribute_id: None, falloff_attribute_id: None,
        tracking_attribute_id: None, modifiers: Vec::new(),
    }
}

#[test]
fn supplement_only_empty_lists_and_preserve_webifier_row() {
    let mut effect = effect("remoteWebifierFalloff");
    supplement(&mut effect);
    assert_eq!(effect.modifiers.len(), 1);
    let row = &effect.modifiers[0];
    assert_eq!((row.func.as_str(), row.domain.as_str(), row.op, row.modified, row.modifying),
        ("ItemModifier", "targetID", POST_PERCENT, ATTR_MAX_VELOCITY, ATTR_SPEED_FACTOR_INTERIM));
    effect.modifiers[0].modifying = 1;
    supplement(&mut effect);
    assert_eq!(effect.modifiers.len(), 1);
    assert_eq!(effect.modifiers[0].modifying, 1);
    let mut unknown = self::effect("unrecognized");
    supplement(&mut unknown);
    assert!(unknown.modifiers.is_empty());
}

#[test]
fn actual_bastion_name_supplements_only_empty_sde_modifiers() {
    let mut bastion = effect("moduleBonusBastionModule");
    bastion.effect_id = 6658;
    supplement(&mut bastion);
    assert!(bastion.modifiers.iter().any(|row|
        row.func == "ItemModifier" && row.domain == "shipID"
            && row.op == POST_PERCENT && row.modified == ATTR_MAX_VELOCITY
            && row.modifying == ATTR_SPEED_FACTOR));

    let mut existing = effect("moduleBonusBastionModule");
    existing.effect_id = 6658;
    existing.modifiers.push(bastion.modifiers.remove(0));
    existing.modifiers[0].modifying = 1;
    supplement(&mut existing);
    assert_eq!(existing.modifiers.len(), 1);
    assert_eq!(existing.modifiers[0].modifying, 1);

    for name in ["marauderModeEffect", "fighterMicroJumpDrive", "fighterAbilityMicroJumpDrive"] {
        let mut excluded = effect(name);
        supplement(&mut excluded);
        assert!(excluded.modifiers.is_empty(), "{name}");
    }

    let mut mjd = effect("microJumpDrive");
    mjd.effect_id = 4921;
    supplement(&mut mjd);
    assert_eq!(mjd.modifiers.len(), 1);
    assert_eq!((mjd.modifiers[0].op, mjd.modifiers[0].modified, mjd.modifiers[0].modifying),
        (POST_PERCENT, ATTR_SIGNATURE_RADIUS, ATTR_SIGNATURE_RADIUS_BONUS_PERCENT));
}

#[test]
fn migrated_skills_keep_required_source_skill_and_operations() {
    for (name, func, domain, modified, modifying) in [
        ("missileEMDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EM_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileExplosiveDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_EXPLOSIVE_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileKineticDmgBonus2", "OwnerRequiredSkillModifier", "charID", ATTR_KINETIC_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("missileThermalDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_THERMAL_DAMAGE, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("droneDmgBonus", "OwnerRequiredSkillModifier", "charID", ATTR_DAMAGE_MULTIPLIER, ATTR_DAMAGE_MULTIPLIER_BONUS),
        ("cloakingTargetingDelayBonusPostPercentCloakingTargetingDelayBonusForShipModulesRequiringCloaking", "LocationRequiredSkillModifier", "shipID", ATTR_CLOAKING_TARGETING_DELAY, ATTR_CLOAKING_TARGETING_DELAY_BONUS),
        ("selfRof", "LocationRequiredSkillModifier", "shipID", ATTR_RATE_OF_FIRE, ATTR_ROF_BONUS),
    ] {
        let mut effect = effect(name);
        supplement(&mut effect);
        assert_eq!(effect.modifiers.len(), 1);
        let row = &effect.modifiers[0];
        assert_eq!((row.func.as_str(), row.domain.as_str(), row.op, row.modified, row.modifying, row.skill),
            (func, domain, POST_PERCENT, modified, modifying, Some(SOURCE_ITEM_TYPE)));
    }
}

#[test]
fn propulsion_rows_cover_mass_and_microwarp_signature_once() {
    let mut afterburner = effect("moduleBonusAfterburner");
    let mut microwarpdrive = effect("moduleBonusMicrowarpdrive");
    supplement(&mut afterburner);
    supplement(&mut microwarpdrive);
    assert_eq!(afterburner.modifiers.len(), 1);
    assert_eq!((afterburner.modifiers[0].op, afterburner.modifiers[0].modified, afterburner.modifiers[0].modifying),
        (MOD_ADD, ATTR_MASS, ATTR_MASS_ADDITION));
    assert_eq!(microwarpdrive.modifiers.len(), 2);
    assert_eq!((microwarpdrive.modifiers[0].op, microwarpdrive.modifiers[0].modified), (MOD_ADD, ATTR_MASS));
    assert_eq!((microwarpdrive.modifiers[1].op, microwarpdrive.modifiers[1].modified, microwarpdrive.modifiers[1].modifying),
        (POST_PERCENT, ATTR_SIGNATURE_RADIUS, ATTR_SIGNATURE_RADIUS_BONUS));
}
