use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// Two ways in. Locally, one DATABASE_URL as .env.example shows. In the cluster, the libpq
/// variables: host/port/username/password come from the owner's Postgres Secret unchanged,
/// and PGDATABASE names this app's own database, which is the only part that differs between
/// environments. Keeping them separate means no password is ever substituted into a URL, so
/// a character like @ or / in it cannot corrupt the string.
///
/// Panics on a missing configuration rather than returning an error: there is no useful
/// degraded mode, and a pod that exits at startup is easier to diagnose than one that
/// answers probes and fails every request.
pub fn pg_pool(max_connections: u32) -> PgPool {
    let options = if let Ok(url) = std::env::var("DATABASE_URL") {
        url.parse::<PgConnectOptions>()
            .expect("DATABASE_URL is not a valid Postgres connection string")
    } else if std::env::var_os("PGDATABASE").is_some() {
        PgConnectOptions::new()
    } else {
        // Not defaulted: PgConnectOptions would otherwise fall back to a database named
        // after the connecting user, which is the shared one this app moved off.
        panic!("set DATABASE_URL, or the PG* variables including PGDATABASE");
    };

    // Lazy: the pod comes up and answers probes even if Postgres is briefly unreachable,
    // instead of crashlooping on a transient failure. Connections are opened on first use.
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with(options)
}
