mod watcher;
mod xact_controller;

use bb8::{ErrorSink, Pool};
use bb8_postgres::{tokio_postgres::NoTls, PostgresConnectionManager};
use log::error;

pub use watcher::PgWatcher;
pub use xact_controller::{LocalXactController, SurrogateXactController, XactController};

pub type PgConnectionPool = Pool<PostgresConnectionManager<NoTls>>;

#[derive(Debug, Clone, Copy)]
pub struct PgErrorSink;

type PgConnError = <PostgresConnectionManager<NoTls> as bb8::ManageConnection>::Error;

impl ErrorSink<PgConnError> for PgErrorSink {
    fn sink(&self, e: PgConnError) {
        error!("{}", e);
    }

    fn boxed_clone(&self) -> Box<dyn ErrorSink<PgConnError>> {
        Box::new(*self)
    }
}

pub async fn create_pg_conn_pool(pg_url: &url::Url) -> anyhow::Result<PgConnectionPool> {
    let pg_mgr = PostgresConnectionManager::new_from_stringlike(pg_url.to_string(), NoTls)?;
    // TODO: expose settings for the connection pool
    Ok(Pool::builder()
        .max_size(50)
        .error_sink(Box::new(PgErrorSink))
        .build(pg_mgr)
        .await?)
}
