use super::*;
use crate::{consts::{CATEGORY_ENTITY, EFFECT_NPC_BEHAVIOR_SMARTBOMB, ATTR_SHIELD_CAPACITY},
    services::combat::groups_health::npc_health_fraction,
    space::{NpcAction, NpcBrain, NpcConfig, World, HOSTILE_TO_ALL}};

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn npc_smartbomb_rejects_stale_generation_and_hits_nearby_nonself() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let effect = super::super::super::registry::resolve(&sde, EFFECT_NPC_BEHAVIOR_SMARTBOMB).unwrap();
    let source_type = sde.types.iter().find(|ty| ty.effects().iter().any(|e| e.effect_id == EFFECT_NPC_BEHAVIOR_SMARTBOMB)
        && ty.attribute(ATTR_EMP_FIELD_RANGE).is_some_and(|v| v > 0.0)
        && ATTR_DAMAGE.into_iter().any(|attr| ty.attribute(attr).is_some_and(|v| v > 0.0)))
        .expect("NPC smartbomb source with damage and radius").type_id;
    let target_type = sde.types.iter().find(|ty| ty.category_id == Some(CATEGORY_ENTITY)
        && ty.attribute(ATTR_SHIELD_CAPACITY).is_some_and(|v| v > 0.0)).unwrap().type_id;
    let radius = sde.types.attribute(source_type, ATTR_EMP_FIELD_RANGE).unwrap();
    let world = World::new(db.clone(), sde, 1);
    let system = -32001;
    let source = -3200100;
    let hostile = source + 1;
    let nearby = source + 2;
    let far = source + 3;
    world.system(system).await.unwrap();
    let (shots, mut shot_rx) = tokio::sync::mpsc::unbounded_channel();
    world.with(system, move |s| {
        let mut me = ball(source, 0.0, None); me.type_id = source_type;
        let mut target = ball(hostile, 0.0, None); target.type_id = target_type; target.signature_radius = Some(1.0);
        let mut other = ball(nearby, 0.0, None); other.type_id = target_type;
        let mut outside = ball(far, radius + 1.0, None); outside.type_id = target_type;
        for b in [me, target, other, outside] { s.insert(b); }
        let mut brain = NpcBrain::new(Some(HOSTILE_TO_ALL), NpcConfig {
            fly_range: 1.0, aggro_range: 1000.0, rate_of_fire_ms: 1e9,
            scan_resolution: 17_163.0, repairs: (0.0, 0.0, 1000.0),
        }, Arc::new(move |action| {
            if let NpcAction::Shot { generation, .. } = action { let _ = shots.send(generation); }
            Box::pin(async {})
        }));
        brain.target = Some((hostile, 1, 0)); brain.announced = true;
        s.add_npc(source, brain);
    }).await.unwrap();
    // This manually constructed brain has no registered effects; its shot exposes
    // the actual process-unique generation without starting an automatic smartbomb.
    let generation = tokio::time::timeout(std::time::Duration::from_secs(30), shot_rx.recv())
        .await.expect("NPC shot timed out").expect("NPC shot generation");
    let me = world.ball(system, source).await.unwrap();
    let victim = world.ball(system, hostile).await.unwrap();
    apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, 0).await.ok().expect("apply stale NPC smartbomb");
    let before = world.with(system, move |s| npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap()).await.unwrap();
    assert_eq!(before, 1.0);
    apply_npc(&db, &world, system, 0, &me, &victim, &effect, 1.0, generation).await.ok().expect("apply NPC smartbomb");
    let (near, distant, own) = world.with(system, move |s| (
        npc_health_fraction(s.ball(nearby).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(far).unwrap(), 0).unwrap(),
        npc_health_fraction(s.ball(source).unwrap(), 0).unwrap(),
    )).await.unwrap();
    assert!(near < before);
    assert_eq!(distant, 1.0);
    assert_eq!(own, 1.0);
}
