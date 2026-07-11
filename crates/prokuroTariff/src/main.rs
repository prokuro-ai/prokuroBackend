use std::{env, net::SocketAddr, process, sync::Arc};

use tokio::signal::unix::{SignalKind, signal};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let data = match prokuro_tariff::data::TariffData::load() {
        Ok(data) => Arc::new(data),
        Err(error) => {
            tracing::error!(%error, "refusing to start: official tariff data failed to load");
            process::exit(1);
        }
    };

    data.log_staleness_warnings(chrono::Utc::now().date_naive());

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3003);
    let address = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, %address, "failed to bind");
            process::exit(1);
        }
    };

    let state = prokuro_tariff::AppState { data };
    tracing::info!(%address, "prokuro-tariff listening");

    if let Err(error) = axum::serve(listener, prokuro_tariff::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(%error, "server error");
        process::exit(1);
    }
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
