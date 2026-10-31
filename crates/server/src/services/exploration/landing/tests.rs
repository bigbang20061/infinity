use super::*;

#[test]
fn reservation_is_once_and_failure_only_releases_empty() {
    let mut slot = None;
    assert!(reserve(&mut slot));
    assert!(!reserve(&mut slot));
    release(&mut slot);
    assert!(reserve(&mut slot));
    slot = Some(vec![42]);
    release(&mut slot);
    assert_eq!(slot, Some(vec![42]));
    assert!(!reserve(&mut slot));
}
