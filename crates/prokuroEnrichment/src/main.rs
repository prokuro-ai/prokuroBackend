use std::{env, net::SocketAddr};

use tokio::signal::unix::{signal, SignalKind};

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt::init();

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3002);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    tracing::info!(%address, "prokuro-enrichment listening");
    axum::serve(listener, prokuro_enrichment::app())
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
