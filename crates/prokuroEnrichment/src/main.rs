use std::{env, net::SocketAddr};

use tokio::signal::unix::{SignalKind, signal};

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt::init();

    let cache = match env::var("DATABASE_URL") {
        Ok(url) => match sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
        {
            Ok(pool) => match sqlx::migrate!("./migrations").run(&pool).await {
                Ok(()) => {
                    tracing::info!("part cache enabled");
                    Some(pool)
                }
                Err(error) => {
                    tracing::warn!(%error, "part cache migrations failed; continuing without cache");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(%error, "DATABASE_URL set but connection failed; continuing without cache");
                None
            }
        },
        Err(_) => {
            tracing::warn!("DATABASE_URL unset; part cache disabled");
            None
        }
    };

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3002);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    let state = prokuro_enrichment::AppState { cache };
    tracing::info!(%address, "prokuro-enrichment listening");
    axum::serve(
        listener,
        prokuro_enrichment::app(state),
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
