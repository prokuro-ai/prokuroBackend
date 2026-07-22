use std::{env, net::SocketAddr, sync::Arc};

use prokuro_enrichment::metrics;
use prokuro_enrichment::providers::DigiKeyProvider;
use prokuro_enrichment::store::PartStore;
use prokuro_enrichment::types::Provider;
use prokuro_enrichment::AppState;
use prokuro_enrichment::sync;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    metrics::init();

    let store = PartStore::from_env().await?;
    let provider: Arc<dyn Provider> = Arc::new(DigiKeyProvider::from_env()?);
    sync::spawn(store.clone(), Arc::clone(&provider));

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3002);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    let state = AppState {
        store: Arc::new(store),
        provider,
    };
    tracing::info!(%address, "prokuro-enrichment listening");
    axum::serve(listener, prokuro_enrichment::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
