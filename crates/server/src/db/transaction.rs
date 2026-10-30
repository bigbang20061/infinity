//! Atomic game and SDE database operations with bounded conflict retries.
//! This server-side facility has no client counterpart; it does not retry individual statements.
//! If rollback fails, the original operation error is returned without retrying
//! because the transaction's outcome is uncertain; the rollback failure is logged.

use std::{future::Future, pin::Pin};
use tokio_postgres::error::SqlState;
use super::{Db, Error};

/// Server design: retry one complete transaction after a serialization conflict or deadlock.
const MAX_TRANSACTION_RETRIES: usize = 1;

pub type TxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

pub trait TransactionError: From<Error> {
    fn is_retryable_transaction(&self) -> bool;
}

pub(crate) fn retryable_sqlstate(code: Option<&SqlState>) -> bool {
    code.is_some_and(|code| *code == SqlState::T_R_DEADLOCK_DETECTED || *code == SqlState::T_R_SERIALIZATION_FAILURE)
}

pub(crate) fn retryable_postgres(error: &tokio_postgres::Error) -> bool {
    retryable_sqlstate(error.code())
}

impl TransactionError for Error {
    fn is_retryable_transaction(&self) -> bool {
        match self {
            Error::Postgres(error) => retryable_postgres(error),
            Error::Pool(_) => false,
        }
    }
}

impl Db {
    pub async fn transaction<T, R, E, O, F, D, DFut>(&self, mut operation: F, dispatch: D) -> Result<R, E>
    where
        T: Send,
        R: Send,
        E: TransactionError + Send,
        O: Default + Send,
        F: for<'call, 'tx> FnMut(&'call tokio_postgres::Transaction<'tx>, &'call mut O) -> TxFuture<'call, T, E> + Send,
        D: FnOnce(T, O) -> DFut + Send,
        DFut: Future<Output = Result<R, E>> + Send,
    {
        let mut retries = 0;
        let (value, outbox) = loop {
            let mut connection = self.pool.get().await.map_err(Error::from).map_err(E::from)?;
            let tx = connection.transaction().await.map_err(Error::from).map_err(E::from)?;
            let mut outbox = O::default();
            let value = match operation(&tx, &mut outbox).await {
                Ok(value) => value,
                Err(error) => {
                    if let Err(rollback_error) = tx.rollback().await {
                        tracing::warn!("transaction rollback failed: {rollback_error}");
                        return Err(error);
                    }
                    if retries < MAX_TRANSACTION_RETRIES && error.is_retryable_transaction() {
                        retries += 1;
                        continue;
                    }
                    return Err(error);
                }
            };
            match tx.commit().await {
                Ok(()) => {
                    drop(connection);
                    break (value, outbox);
                }
                Err(error) => {
                    let error = E::from(Error::from(error));
                    if retries < MAX_TRANSACTION_RETRIES && error.is_retryable_transaction() {
                        retries += 1;
                        continue;
                    }
                    return Err(error);
                }
            }
        };
        dispatch(value, outbox).await
    }
}
#[cfg(test)] mod tests;
