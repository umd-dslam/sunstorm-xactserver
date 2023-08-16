use async_trait::async_trait;
use tokio_postgres::types::ToSql;
use tokio_postgres::Error;
use tracing::info;

pub struct FakePostgresConnectionManager<Tls> {
    _tls: Tls,
}

impl<Tls> FakePostgresConnectionManager<Tls> {
    pub fn new_from_stringlike<T>(
        _params: T,
        tls: Tls,
    ) -> Result<FakePostgresConnectionManager<Tls>, Error>
    where
        T: ToString,
    {
        Ok(FakePostgresConnectionManager { _tls: tls })
    }
}

pub struct FakeClient;

impl FakeClient {
    pub async fn execute(
        &self,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, Error> {
        info!(statement, ?params);
        Ok(0)
    }

    pub async fn batch_execute(&self, query: &str) -> Result<(), Error> {
        info!(query);
        Ok(())
    }

    pub async fn query_opt(
        &self,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<()>, Error> {
        info!(statement, ?params);
        Ok(Some(()))
    }
}

#[async_trait]
impl<Tls> bb8::ManageConnection for FakePostgresConnectionManager<Tls>
where
    Tls: Send + Sync + 'static,
{
    type Connection = FakeClient;
    type Error = Error;

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        Ok(FakeClient)
    }

    async fn is_valid(&self, _conn: &mut Self::Connection) -> Result<(), Self::Error> {
        Ok(())
    }

    fn has_broken(&self, _conn: &mut Self::Connection) -> bool {
        false
    }
}
