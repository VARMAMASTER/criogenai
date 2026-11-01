//! A fresh database per test, on the server named by
//! `CRIOGEN_TEST_POSTGRES_URL` (`scripts/with-postgres.sh` starts a
//! throwaway one). Without it the Postgres tests skip; with
//! `CRIOGEN_REQUIRE_POSTGRES=1` they fail instead.

use sqlx::Connection;

pub struct TestDb {
    admin: String,
    name: String,
    /// The test database's URL.
    pub url: String,
}

impl TestDb {
    /// A new, empty database; `None` if no server is configured.
    pub async fn create() -> Option<Self> {
        let Ok(admin) = std::env::var("CRIOGEN_TEST_POSTGRES_URL") else {
            assert!(
                std::env::var_os("CRIOGEN_REQUIRE_POSTGRES").is_none(),
                "CRIOGEN_REQUIRE_POSTGRES is set but CRIOGEN_TEST_POSTGRES_URL is not"
            );
            eprintln!("skipped: no CRIOGEN_TEST_POSTGRES_URL (see scripts/with-postgres.sh)");
            return None;
        };
        let name = format!("criogen_test_{}", uuid::Uuid::now_v7().simple());
        let mut conn = sqlx::PgConnection::connect(&admin).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
        Some(Self {
            url: with_database(&admin, &name),
            admin,
            name,
        })
    }

    /// A direct connection pool, to look at rows.
    #[allow(dead_code)] // not every test file looks
    pub async fn raw(&self) -> sqlx::PgPool {
        sqlx::PgPool::connect(&self.url).await.unwrap()
    }

    /// Drops the database (and any connections left open to it).
    pub async fn remove(self) {
        let mut conn = sqlx::PgConnection::connect(&self.admin).await.unwrap();
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.name))
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
    }
}

/// `url` with its database replaced by `name`.
pub fn with_database(url: &str, name: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (url, None),
    };
    let authority = base.find("://").expect("a URL") + 3;
    let path = base[authority..]
        .find('/')
        .map_or(base.len(), |i| authority + i);
    let mut out = format!("{}/{name}", &base[..path]);
    if let Some(query) = query {
        out.push('?');
        out.push_str(query);
    }
    out
}
