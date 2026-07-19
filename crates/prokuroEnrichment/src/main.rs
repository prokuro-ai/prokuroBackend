use std::{env, net::SocketAddr, sync::Arc};

use tokio::signal::unix::{SignalKind, signal};

use prokuro_enrichment::AppState;
use prokuro_enrichment::providers::DigiKeyProvider;
use prokuro_enrichment::store::PartStore;
use prokuro_enrichment::types::Provider;
use prokuro_enrichment::{sync, worker};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let store = PartStore::from_env().await?;
    let provider: Arc<dyn Provider> = Arc::new(DigiKeyProvider::from_env()?);
    let (tx, rx) = worker::channel(10_000);
    worker::spawn(store.clone(), Arc::clone(&provider), rx);
    sync::spawn(store.clone(), Arc::clone(&provider));

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3002);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    let state = AppState {
        store: Arc::new(store),
        enrich_tx: tx,
    };
    tracing::info!(%address, "prokuro-enrichment listening");
    axum::serve(listener, prokuro_enrichment::app(state))
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
