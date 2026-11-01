use super::*;

#[test]
fn strict_overload_arguments() {
    assert_eq!(arguments(&[Value::Int(10), Value::Int(3001)], false), Some((10, Some(3001))));
    assert_eq!(arguments(&[Value::Int(10)], true), Some((10, None)));
    for args in [vec![], vec![Value::Int(0)], vec![Value::Float(10.0)], vec![Value::Bool(true)], vec![Value::Int(10), Value::Int(1)]] {
        assert_eq!(arguments(&args, true), None);
    }
    for args in [vec![Value::Int(10)], vec![Value::Int(-1), Value::Int(1)], vec![Value::Int(10), Value::Float(1.0)], vec![Value::Int(10), Value::Int(i64::MAX)]] {
        assert_eq!(arguments(&args, false), None);
    }
}


#[test]
fn single_and_rack_return_shapes() {
    let modules = vec![(Module { id: 10, type_id: 20, flag: 11 }, 3001), (Module { id: 12, type_id: 20, flag: 12 }, 3001)];
    assert_eq!(result_value(false, &modules), Value::None);
    assert_eq!(result_value(true, &modules), Value::list([Value::Int(10), Value::Int(12)]));
}
#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn sde_types_have_at_most_one_overload_effect() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.unwrap();
    for ty in sde.types.iter() {
        assert!(ty.effects().iter().filter(|effect| sde.dogma.effect(effect.effect_id)
            .is_some_and(|row| row.effect_category == Some(OVERLOAD_CATEGORY))).count() <= 1, "type {}", ty.type_id);
    }
}
