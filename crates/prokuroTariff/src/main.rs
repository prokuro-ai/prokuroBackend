use std::{
    env,
    net::SocketAddr,
    process,
    sync::{Arc, RwLock},
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let data = match prokuro_tariff::data::TariffData::load().await {
        Ok(data) => Arc::new(RwLock::new(data)),
        Err(error) => {
            tracing::error!(%error, "refusing to start: official tariff data failed to load");
            process::exit(1);
        }
    };

    {
        let guard = data.read().expect("tariff data lock poisoned");
        guard.log_staleness_warnings(chrono::Utc::now().date_naive());
    }

    prokuro_tariff::spawn_dataset_reload_task(data.clone());

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
