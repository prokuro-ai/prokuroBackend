use std::{env, net::SocketAddr, time::Duration};

use axum::{extract::MatchedPath, http::Request};
use tokio::signal::unix::{signal, SignalKind};
use tower_http::trace::TraceLayer;
use tracing::info_span;

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt::init();

    let state = prokuro_gateway::build_app_state().await;
    let app = prokuro_gateway::app((*state).clone());

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3000);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    tracing::info!(%address, "prokuro-gateway listening");
    axum::serve(
        listener,
        app.layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    let path = request
                        .extensions()
                        .get::<MatchedPath>()
                        .map(MatchedPath::as_str)
                        .unwrap_or_else(|| request.uri().path());

                    info_span!(
                        "http_request",
                        request_id = %request_id,
                        method = %request.method(),
                        path = %path
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>,
                     latency: Duration,
                     _span: &tracing::Span| {
                        tracing::info!(
                            status = %response.status().as_u16(),
                            latency_ms = %latency.as_millis(),
                            "request finished"
                        );
                    },
                ),
        ),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
}

async fn shutdown_signal() {
    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
            tracing::info!("SIGTERM received, shutting down");
        }
        Err(error) => {
            tracing::warn!(%error, "failed to install SIGTERM handler");
        }
    }
}
