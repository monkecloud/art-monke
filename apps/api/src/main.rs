use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{routing::get, routing::post, Json, Router};
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::signal::unix::{signal, SignalKind};

#[derive(Serialize)]
struct Visits {
    visits: i64,
}

// A failed query is the api's problem, not the caller's, so the body stays generic and the
// detail goes to the log where it is not exposed to the internet.
struct ApiError(sqlx::Error);

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        eprintln!("query failed: {}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "database unavailable\n").into_response()
    }
}

// One statement, so two replicas incrementing at once cannot lose an update the way a
// read-then-write pair would. The row is created on first call rather than seeded by the
// migration, which keeps the migration a pure schema change.
async fn visit(State(pool): State<PgPool>) -> Result<Json<Visits>, ApiError> {
    let (value,): (i64,) = sqlx::query_as(
        "INSERT INTO counters (name, value) VALUES ($1, 1)
         ON CONFLICT (name) DO UPDATE SET value = counters.value + 1
         RETURNING value",
    )
    .bind("page_loads")
    .fetch_one(&pool)
    .await?;

    Ok(Json(Visits { visits: value }))
}

// Deliberately does not touch Postgres. A probe that queries the database turns one blip
// into every replica failing readiness at once, which is an outage the blip did not cause.
async fn healthz() -> &'static str {
    "ok\n"
}

#[tokio::main]
async fn main() {
    // Two ways in. Locally, one DATABASE_URL as .env.example shows. In the cluster, the
    // libpq variables: host/port/username/password come from the owner's Postgres Secret
    // unchanged, and PGDATABASE names this app's own database, which is the only part that
    // differs between environments. Keeping them separate means no password is ever
    // substituted into a URL, so a character like @ or / in it cannot corrupt the string.
    let options = if let Ok(url) = std::env::var("DATABASE_URL") {
        url.parse::<PgConnectOptions>()
            .expect("DATABASE_URL is not a valid Postgres connection string")
    } else if std::env::var_os("PGDATABASE").is_some() {
        PgConnectOptions::new()
    } else {
        // Not defaulted: PgConnectOptions would otherwise fall back to a database named
        // after the connecting user, which is the shared one this app just moved off.
        panic!("set DATABASE_URL, or the PG* variables including PGDATABASE");
    };

    // Lazy: the pod comes up and answers probes even if Postgres is briefly unreachable,
    // instead of crashlooping on a transient failure. Connections are opened on first use.
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with(options);

    // Migrations take a Postgres advisory lock, so both replicas starting together is safe:
    // one applies, the other waits and finds nothing to do. Retried because a pod can start
    // while Postgres is still coming back, and a blocked egress hangs rather than erroring.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match sqlx::migrate!("./migrations").run(&pool).await {
            Ok(()) => break,
            Err(e) if attempt < 5 => {
                eprintln!("migration attempt {attempt} failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            // Give up loudly. With maxUnavailable: 0 the old pods keep serving, so a
            // crashloop stalls the rollout rather than taking the api down.
            Err(e) => panic!("migrations failed after {attempt} attempts: {e}"),
        }
    }

    let app = Router::new()
        .route("/healthz", get(healthz))
        // POST because it mutates: a GET would be incremented by every prefetcher, crawler
        // and probe that touches the URL.
        .route("/api/visits", post(visit))
        .with_state(pool);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    // 0.0.0.0, not loopback: a container bound to 127.0.0.1 is unreachable from the pod
    // network and every probe fails.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    eprintln!("monke-api listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .expect("server error");
}

// Kubernetes sends SIGTERM and waits before killing. Without this the process dies
// immediately and in-flight requests are dropped on every rolling update.
async fn shutdown() {
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    term.recv().await;
    eprintln!("SIGTERM received, draining");
}
