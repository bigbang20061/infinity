use super::*;

#[test]
fn removing_jammed_ball_clears_state_and_old_timer_cannot_end_new_jam() {
    use crate::services::combat::jamming::{apply, is_jammed};
    let (target, source, module, victim, attacker) = (-810001, -810002, -810003, -810004, -810005);
    let registry = Arc::new(Pilots::default());
    let (victim_tx, mut victim_rx) = mpsc::unbounded_channel();
    let (source_tx, mut source_rx) = mpsc::unbounded_channel();
    registry.register_pilot(victim, victim_tx); registry.register_pilot(attacker, source_tx);
    let mut s = System::new(target, registry, 1000);
    s.insert(ball(target, Some(victim), 0.0)); s.insert(ball(source, Some(attacker), 0.0));
    apply(&mut s, source, module, target, 100.0);
    for rx in [&mut victim_rx, &mut source_rx] { assert_eq!(rx.try_recv().unwrap().0, "OnJamStart"); assert_eq!(rx.try_recv().unwrap().0, "OnEwarStart"); }
    assert!(s.remove_ball(target).is_some());
    for rx in [&mut victim_rx, &mut source_rx] { assert_eq!(rx.try_recv().unwrap().0, "OnJamEnd"); assert_eq!(rx.try_recv().unwrap().0, "OnEwarEnd"); }
    s.insert(ball(target, Some(victim), 0.0));
    assert!(!is_jammed(target, s.now_ms()));
    apply(&mut s, source, module, target, 300.0);
    for rx in [&mut victim_rx, &mut source_rx] { assert_eq!(rx.try_recv().unwrap().0, "OnJamStart"); assert_eq!(rx.try_recv().unwrap().0, "OnEwarStart"); }
    s.tick(1100);
    assert!(is_jammed(target, s.now_ms()));
    for rx in [&mut victim_rx, &mut source_rx] { assert!(rx.try_recv().is_err()); }
    s.tick(1300);
    assert!(!is_jammed(target, s.now_ms()));
    for rx in [&mut victim_rx, &mut source_rx] { assert_eq!(rx.try_recv().unwrap().0, "OnJamEnd"); assert_eq!(rx.try_recv().unwrap().0, "OnEwarEnd"); assert!(rx.try_recv().is_err()); }
}
