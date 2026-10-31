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
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
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
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
    let mut third = register(account, true).unwrap();
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
    let first = register(-81003, true).unwrap();
    let mut other = register(-81004, true).unwrap();
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
    let first = register(account, true).unwrap();
    let mut second = register(account, true).unwrap();
    drop(first);
    assert!(contains(account));
    timeout(Duration::from_secs(1), second.wait_for_predecessor()).await.unwrap();
    let mut third = register(account, true).unwrap();
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
    let first = register(account, true).unwrap();
    let second = register(account, true).unwrap();
    drop(second);
    assert!(contains(account));
    let mut third = register(account, true).unwrap();
    still_waiting(&mut third).await;
    drop(first);
    timeout(Duration::from_secs(1), third.wait_for_predecessor()).await.unwrap();
    drop(third);
    assert!(!contains(account));
}

#[test]
fn disabled_takeover_refuses_duplicates_without_cancelling() {
    let account = -81007;
    let first = register(account, false).unwrap();
    assert!(register(account, false).is_none());
    assert!(!first.is_cancelled());
    drop(first);
    assert!(!contains(account));
}

#[test]
fn disabled_takeover_respects_dropped_pending_barrier() {
    let account = -81008;
    let first = register(account, false).unwrap();
    let pending = register(account, true).unwrap();
    drop(pending);
    assert!(register(account, false).is_none());
    drop(first);
    let next = register(account, false).unwrap();
    drop(next);
    assert!(!contains(account));
}

#[test]
fn concurrent_disabled_takeovers_have_one_winner() {
    let account = -81009;
    let start = std::sync::Arc::new(std::sync::Barrier::new(3));
    let finish = std::sync::Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2).map(|_| {
        let start = start.clone();
        let finish = finish.clone();
        std::thread::spawn(move || {
            start.wait();
            let registration = register(account, false);
            finish.wait();
            registration.is_some()
        })
    }).collect();
    start.wait();
    finish.wait();
    assert_eq!(threads.into_iter().map(|thread| thread.join().unwrap()).filter(|won| *won).count(), 1);
    assert!(!contains(account));
}
