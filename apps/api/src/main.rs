use std::net::SocketAddr;

use axum::{routing::get, Json, Router};
use serde::Serialize;
use tokio::signal::unix::{signal, SignalKind};

#[derive(Serialize)]
struct Placeholder {
    service: &'static str,
    status: &'static str,
    bananas: u32,
}

// Served under /api because the Ingress routes by path prefix and does not strip it.
async fn hello() -> Json<Placeholder> {
    Json(Placeholder {
        service: "monke-api",
        status: "placeholder",
        bananas: 3,
    })
}

// Probed by the Deployment, so it deliberately sits outside /api and is never routed
// publicly. Keep it dependency-free: it answers whether this process can serve, not
// whether Postgres is up, or a blip downstream would restart every pod at once.
async fn healthz() -> &'static str {
    "ok\n"
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/hello", get(hello));

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
