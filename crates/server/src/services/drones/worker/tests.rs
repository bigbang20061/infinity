use super::*;

#[test]
fn admission_accepts_supported_drone_families_but_not_salvage() {
    for kind in [EffectKind::Turret, EffectKind::Missile, EffectKind::Mining,
        EffectKind::RemoteRepair { layer: 0 }, EffectKind::RemoteRepair { layer: 1 },
        EffectKind::RemoteRepair { layer: 2 }, EffectKind::Neutralizer, EffectKind::Ecm,
        EffectKind::Projected { jamming: JammingType::Webify },
        EffectKind::Projected { jamming: JammingType::Paint },
        EffectKind::Projected { jamming: JammingType::Damp },
        EffectKind::Projected { jamming: JammingType::TrackingDisrupt }] {
        assert!(supported(kind), "{kind:?}");
    }
    for kind in [EffectKind::Salvage, EffectKind::Smartbomb, EffectKind::Modifiers,
        EffectKind::Projected { jamming: JammingType::Scramble }] {
        assert!(!supported(kind), "{kind:?}");
    }
}

#[tokio::test]
#[ignore = "requires a dedicated database with the retail SDE"]
async fn persisted_drone_types_always_have_a_physical_spec() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let mut salvage = 0;
    let mut turret = 0;
    for ty in sde.types.iter().filter(|ty| ty.category_id == Some(crate::consts::CATEGORY_DRONE)) {
        let fit = Fit::for_space_type(sde.clone(), ty.type_id, Vec::new());
        let spec = spec(&fit, Key::Ship, ty.type_id).unwrap();
        assert_eq!(spec.type_id, ty.type_id);
        let kinds: Vec<_> = sde.types.effects(ty.type_id).iter()
            .filter_map(|row| registry::resolve(&sde, row.effect_id).map(|effect| effect.kind)).collect();
        if kinds.contains(&EffectKind::Salvage) && !kinds.iter().copied().any(supported) {
            assert!(spec.effect_id.is_none());
            salvage += 1;
        }
        if kinds.contains(&EffectKind::Turret) && spec.effect_id.is_some() { turret += 1; }
    }
    assert!(salvage > 0 && turret > 0);
}
