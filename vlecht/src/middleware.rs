use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use std::time::Instant;
use tracing::info;
use axum::body::Body;
use axum::http::Request;

/// CORS middleware matching the Go knotserver's `middleware.go:37-53`.
pub async fn cors_middleware(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Content-Type, Authorization".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        "86400".parse().unwrap(),
    );
    response
}

/// Request logger middleware matching Go knotserver's `middleware.go:9-35`.
pub async fn request_logger(req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();
    let response = next.run(req).await;
    let duration = start.elapsed();
    let status = response.status();
    info!(
        method = %method,
        path = %uri.path(),
        query = %uri.query().unwrap_or(""),
        status = status.as_u16(),
        duration_ms = duration.as_millis(),
        "request",
    );
    response
}
