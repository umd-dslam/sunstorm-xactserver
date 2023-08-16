mod watcher;
mod xact_controller;

pub use watcher::PgWatcher;
pub use xact_controller::{LocalXactController, SurrogateXactController, XactController};

use bb8::{ErrorSink, Pool};
use bb8_postgres::{tokio_postgres::NoTls, PostgresConnectionManager};
use log::error;

type PgConnectionError = <PostgresConnectionManager<NoTls> as bb8::ManageConnection>::Error;

#[derive(Debug, Clone, Copy)]
struct PgErrorSink;

impl ErrorSink<PgConnectionError> for PgErrorSink {
    fn sink(&self, e: PgConnectionError) {
        error!("{}", e);
    }

    fn boxed_clone(&self) -> Box<dyn ErrorSink<PgConnectionError>> {
        Box::new(*self)
    }
}

pub type PgConnectionPool = Pool<PostgresConnectionManager<NoTls>>;

pub async fn create_pg_conn_pool(
    url: &url::Url,
    max_pool_size: u32,
) -> anyhow::Result<PgConnectionPool> {
    let pg_mgr = PostgresConnectionManager::new_from_stringlike(url.to_string(), NoTls)?;

    Ok(Pool::builder()
        .max_size(max_pool_size)
        .error_sink(Box::new(PgErrorSink))
        .build(pg_mgr)
        .await?)
}
