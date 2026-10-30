use super::*;
use std::time::Duration;
use tokio::time::timeout;

fn contains(account_id: i64) -> bool {
    state().lock().unwrap().accounts.contains_key(&account_id)
}

async fn still_waiting(registration: &mut Registration) {
    assert!(timeout(Duration::from_millis(20), registration.wait_for_predecessor()).await.is_err());
}

#[tokio::test]
async fn replacement_cancels_and_waits_for_completion() {
    let account = -81001;
    let first = register(account);
    let mut second = register(account);
    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    still_waiting(&mut second).await;
    drop(first);
    timeout(Duration::from_secs(1), second.wait_for_predecessor()).await.unwrap();
    drop(second);
    assert!(!contains(account));
}

#[tokio::test]
async fn pending_replacement_cannot_be_bypassed_even_when_dropped() {
    let account = -81002;
    let first = register(account);
    let mut second = register(account);
    let mut third = register(account);
    assert!(second.is_cancelled());
    timeout(Duration::from_secs(1), second.cancelled()).await.unwrap();
    still_waiting(&mut second).await;
    still_waiting(&mut third).await;
    drop(second);
    still_waiting(&mut third).await;
    drop(first);
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[tokio::test]
async fn unrelated_accounts_do_not_wait_for_each_other() {
    let first = register(-81003);
    let mut other = register(-81004);
    timeout(Duration::from_secs(1), other.wait_for_predecessor()).await.unwrap();
    assert!(!first.is_cancelled());
    drop(first);
    drop(other);
    assert!(!contains(-81003));
    assert!(!contains(-81004));
}

#[tokio::test]
async fn old_drop_does_not_remove_newest_registration() {
    let account = -81005;
    let first = register(account);
    let mut second = register(account);
    drop(first);
    assert!(contains(account));
    timeout(Duration::from_secs(1), second.wait_for_predecessor()).await.unwrap();
    let mut third = register(account);
    assert!(second.is_cancelled());
    still_waiting(&mut third).await;
    drop(second);
    assert!(contains(account));
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[tokio::test]
async fn dropped_newest_remains_a_barrier_until_predecessor_finishes() {
    let account = -81006;
    let first = register(account);
    let second = register(account);
    drop(second);
    assert!(contains(account));
    let mut third = register(account);
    still_waiting(&mut third).await;
    drop(first);
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}
