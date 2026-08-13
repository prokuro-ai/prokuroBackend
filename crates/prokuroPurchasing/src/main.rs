use std::{env, net::SocketAddr, sync::Arc};

use tokio::signal::unix::{SignalKind, signal};

use prokuro_purchasing::{AppState, app, default_providers};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3004);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    let state = AppState {
        providers: Arc::new(default_providers()),
    };
    tracing::info!(%address, "prokuro-purchasing listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
