use super::*;
use std::sync::Mutex;
#[test]
fn discarded_cycle_owners_release_only_their_generation() {
    let master = -9011;
    let generation = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    {
        let mut st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let system = System::default();
    system.defer(async move { drop(owner); });
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: generation.clone() });
        st.activation_generation.insert(master, generation.clone());
    }
    let owner = ActivationOwner { master, generation: generation.clone(), armed: true };
    let rejected_post = move |_: &mut System| drop(owner);
    drop(rejected_post);
    {
        let st = state();
        assert!(!st.active.contains_key(&master));
        assert!(!st.activation_generation.contains_key(&master));
    }

    let replacement = Arc::new(AtomicBool::new(false));
    {
        let mut st = state();
        st.active.insert(master, ActiveModule { char_id: 1, stop: replacement.clone() });
        st.activation_generation.insert(master, replacement.clone());
    }
    let owner = ActivationOwner { master, generation, armed: true };
    let mut system = System::default();
    schedule_cycle_end(&mut system, 0, 1000.0, move |_, _| drop(owner));
    drop(system);
    let mut st = state();
    assert!(Arc::ptr_eq(&st.active[&master].stop, &replacement));
    assert!(Arc::ptr_eq(&st.activation_generation[&master], &replacement));
    st.active.remove(&master);
    st.activation_generation.remove(&master);
}


#[test]
fn cycle_deadlines_and_deactivation() {
    let mut s = System::default();
    let completed = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let seen = completed.clone();
    let flag = stop.clone();
    schedule_cycle_end(&mut s, 1000, 500.0, move |s, end| {
        seen.lock().unwrap().push((1, s.now_ms(), end));
        assert!(!cycle_is_final(flag.load(Ordering::Relaxed), false, 3, 1));
        assert!(cycle_is_final(false, true, 3, 1));
        // Model a continuation returning late from the worker.
        s.schedule_at(1700, move |s| {
            assert!(!cycle_should_stop(false, &flag));
            schedule_cycle_end(s, end, 500.0, move |s, end| {
                seen.lock().unwrap().push((2, s.now_ms(), end));
                assert!(cycle_is_final(flag.load(Ordering::Relaxed), false, 3, 2));
                assert!(cycle_should_stop(false, &flag));
            });
        });
    });
    s.tick(1499);
    assert!(completed.lock().unwrap().is_empty());
    s.tick(1500);
    s.tick(1700);
    stop.store(true, Ordering::Relaxed);
    s.tick(1999);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500)]);
    s.tick(2000);
    s.tick(3000);
    assert_eq!(*completed.lock().unwrap(), [(1, 1500, 1500), (2, 2000, 2000)]);

    let old = Arc::new(AtomicBool::new(false));
    let replacement = Arc::new(AtomicBool::new(false));
    let mut active = HashMap::from([(1, ActiveModule { char_id: 1, stop: replacement.clone() })]);
    let mut published = false;
    assert!(!retire_activation(&mut active, 1, &old, || published = true));
    assert!(!published);
    assert!(Arc::ptr_eq(&active[&1].stop, &replacement));
    assert!(retire_activation(&mut active, 1, &replacement, || published = true));
    assert!(published);
    assert!(active.is_empty());
}

#[test]
fn subject_requires_owned_fitted_module_on_permitted_ship() {
    use crate::consts::{FLAG_CARGO, FLAG_FITTING_FIRST};
    use crate::sde::types::TypeEffect;
    let mut hull = Item { item_id: 3, type_id: 1, owner_id: 2, location_id: 4, flag_id: FLAG_HANGAR, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_SHIP), custom_info: None };
    let mut module = Item { item_id: 5, type_id: 1, owner_id: 2, location_id: 3, flag_id: FLAG_FITTING_FIRST, quantity: 1, singleton: true, group_id: 0, category_id: i64::from(CATEGORY_MODULE), custom_info: None };
    let effects = [TypeEffect { effect_id: EFFECT_ONLINE as i32 }];
    let valid = |h: &Item, m: &Item, active, station, entries: &[TypeEffect], require_online, online| valid_subject(h, m, 2, 3, active, station, entries, EFFECT_ONLINE as i32, require_online, online);
    assert!(valid(&hull, &module, Some(3), None, &effects, true, true));
    assert!(valid(&hull, &module, Some(9), Some(4), &effects, false, false));
    assert!(!valid(&hull, &module, Some(9), None, &effects, false, false));
    assert!(!valid(&hull, &module, Some(3), None, &[], false, false));
    assert!(!valid(&hull, &module, Some(3), None, &effects, true, false));
    hull.owner_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    hull.owner_id = 2;
    module.owner_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.owner_id = 2;
    module.location_id = 9;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.location_id = 3;
    module.flag_id = FLAG_CARGO;
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
    module.flag_id = FLAG_FITTING_FIRST;
    module.category_id = i64::from(CATEGORY_SHIP);
    assert!(!valid(&hull, &module, Some(3), None, &effects, false, false));
}
