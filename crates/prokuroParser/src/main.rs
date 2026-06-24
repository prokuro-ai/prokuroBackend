use std::{env, net::SocketAddr, time::Duration};

use axum::{
    extract::{MatchedPath, Multipart},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tokio::signal::unix::{signal, SignalKind};
use tower_http::trace::TraceLayer;
use tracing::info_span;

use prokuro_parser::pipeline::{parse_file, ParseError};

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    tracing_subscriber::fmt::init();

    let port = env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3001);

    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address).await?;

    tracing::info!(%address, "prokuro-parser listening");

    axum::serve(listener, app()).with_graceful_shutdown(shutdown_signal()).await
}

fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/parse", post(parse_handler))
        .layer(
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
                    |response: &axum::http::Response<_>, latency: Duration, _span: &tracing::Span| {
                        tracing::info!(
                            status = %response.status().as_u16(),
                            latency_ms = %latency.as_millis(),
                            "request finished"
                        );
                    },
                ),
        )
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "prokuro-parser"
    }))
}

async fn parse_handler(mut multipart: Multipart) -> impl IntoResponse {
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = String::from("upload");

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or("").to_string();
                match name.as_str() {
                    "file" => {
                        if let Some(fname) = field.file_name() {
                            filename = fname.to_string();
                        }
                        match field.bytes().await {
                            Ok(b) => file_bytes = Some(b.to_vec()),
                            Err(e) => {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({"error": e.to_string()})),
                                )
                                    .into_response()
                            }
                        }
                    }
                    "filename" => {
                        if let Ok(s) = field.text().await {
                            if !s.is_empty() {
                                filename = s;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response()
            }
        }
    }

    let bytes = match file_bytes {
        Some(b) => b,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "missing 'file' field"})),
            )
                .into_response()
        }
    };

    match parse_file(&bytes, &filename).await {
        Ok(result) if result.mapping_confidence < 0.3 => {
            (StatusCode::UNPROCESSABLE_ENTITY, Json(result)).into_response()
        }
        Ok(result) => Json(result).into_response(),
        Err(ParseError::UnsupportedFormat) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": ParseError::UnsupportedFormat.to_string()})),
        )
            .into_response(),
        Err(ParseError::EmptyFile) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": ParseError::EmptyFile.to_string()})),
        )
            .into_response(),
        Err(ParseError::EncodingError) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error": ParseError::EncodingError.to_string()})),
        )
            .into_response(),
        Err(ParseError::InternalError(msg)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": msg})),
        )
            .into_response(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[tokio::test]
    async fn health_returns_200() {
        let response = health().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
