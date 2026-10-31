use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_postgres::{types::ToSql, Row, ToStatement};

struct CountingExecutor(AtomicUsize);

impl Executor for CountingExecutor {
    async fn query<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Vec<Row>, crate::db::Error> { panic!("unexpected query") }
    async fn query_one<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Row, crate::db::Error> { panic!("unexpected query") }
    async fn query_opt<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<Option<Row>, crate::db::Error> { panic!("unexpected query") }
    async fn execute<T: ?Sized + ToStatement + Sync>(&self, _: &T, _: &[&(dyn ToSql + Sync)]) -> Result<u64, crate::db::Error> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(1)
    }
}

#[tokio::test]
async fn absolute_standing_checks_range_before_writing() {
    let db = CountingExecutor(AtomicUsize::new(0));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -10.01, 10.01] {
        assert!(set(&db, 1, 2, value).await.is_err());
    }
    assert_eq!(db.0.load(Ordering::Relaxed), 0);
    for value in [-10.0, 0.0, 10.0] {
        assert!(set(&db, 1, 2, value).await.is_ok());
    }
    assert_eq!(db.0.load(Ordering::Relaxed), 3);
}
