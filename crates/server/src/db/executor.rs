//! Generic statement execution against the game and SDE PostgreSQL tables.
//! This server-side interface does not mirror a client module or manage transactions.

use std::future::Future;
use tokio_postgres::{types::ToSql, Row, ToStatement};
use super::{Db, Error};

pub trait Executor: Sync {
    fn query<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send;
    fn query_one<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send;
    fn query_opt<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send;
    fn execute<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send;
}

impl Executor for Db {
    async fn query<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> { Db::query(self, statement, params).await }
    async fn query_one<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> { Db::query_one(self, statement, params).await }
    async fn query_opt<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, Error> { Db::query_opt(self, statement, params).await }
    async fn execute<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> { Db::execute(self, statement, params).await }
}

impl Executor for tokio_postgres::Transaction<'_> {
    async fn query<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, Error> { tokio_postgres::Transaction::query(self, statement, params).await.map_err(Error::from) }
    async fn query_one<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Row, Error> { tokio_postgres::Transaction::query_one(self, statement, params).await.map_err(Error::from) }
    async fn query_opt<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, Error> { tokio_postgres::Transaction::query_opt(self, statement, params).await.map_err(Error::from) }
    async fn execute<T: ?Sized + ToStatement + Sync>(&self, statement: &T, params: &[&(dyn ToSql + Sync)]) -> Result<u64, Error> { tokio_postgres::Transaction::execute(self, statement, params).await.map_err(Error::from) }
}
