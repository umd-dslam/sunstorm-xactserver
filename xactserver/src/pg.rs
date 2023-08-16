mod controller;
#[cfg(test)]
mod fake;
mod watcher;

pub use controller::{LocalXactController, SurrogateXactController, XactController};
pub use watcher::PgWatcher;

use bb8::{ErrorSink, Pool};
use bb8_postgres::tokio_postgres::NoTls;
#[cfg(not(test))]
use bb8_postgres::PostgresConnectionManager;
#[cfg(test)]
use fake::FakePostgresConnectionManager as PostgresConnectionManager;
use tracing::error;

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
