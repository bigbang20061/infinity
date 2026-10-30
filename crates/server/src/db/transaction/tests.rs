use super::*;
use crate::services::CallError;

#[test]
fn only_deadlocks_and_serialization_failures_retry() {
    assert!(retryable_sqlstate(Some(&SqlState::T_R_DEADLOCK_DETECTED)));
    assert!(retryable_sqlstate(Some(&SqlState::T_R_SERIALIZATION_FAILURE)));
    assert!(!retryable_sqlstate(Some(&SqlState::UNIQUE_VIOLATION)));
    assert!(!retryable_sqlstate(None));
}

#[test]
fn user_and_non_sql_internal_errors_do_not_retry() {
    let user = CallError::User { msg: "refused".into(), dict: Vec::new() };
    let internal = CallError::Internal(anyhow::anyhow!("unrelated failure").context("wrapped"));
    assert!(!user.is_retryable_transaction());
    assert!(!internal.is_retryable_transaction());
}
