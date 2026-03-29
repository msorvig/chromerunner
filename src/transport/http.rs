//! Shared HTTP client helpers (used by all backends).

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpStream;
use tracing::{debug, trace};

use crate::error::{Error, Result};

/// Send an HTTP request and return (status, body bytes).
pub async fn http_request(
    host: &str,
    port: u16,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<(StatusCode, Bytes)> {
    debug!(%method, path, port, "HTTP request");
    if let Some(b) = body {
        trace!(body = %b, "HTTP request body");
    }

    let stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .map_err(|e| Error::Http(format!("connect {}:{}: {}", host, port, e)))?;

    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(stream))
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = match body {
        Some(v) => Bytes::from(serde_json::to_vec(v)?),
        None => Bytes::new(),
    };

    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", format!("{}:{}", host, port));

    if body.is_some() {
        builder = builder.header("Content-Type", "application/json");
    }

    let req = builder
        .body(Full::new(body_bytes))
        .map_err(|e| Error::Http(e.to_string()))?;

    let response: hyper::Response<Incoming> = sender
        .send_request(req)
        .await
        .map_err(|e| Error::Http(e.to_string()))?;

    let status = response.status();
    let resp_body = response
        .into_body()
        .collect()
        .await
        .map_err(|e| Error::Http(e.to_string()))?
        .to_bytes();

    debug!(%status, body_len = resp_body.len(), "HTTP response");
    trace!(body = %String::from_utf8_lossy(&resp_body), "HTTP response body");

    Ok((status, resp_body))
}

/// HTTP GET returning parsed JSON.
pub async fn get_json(host: &str, port: u16, path: &str) -> Result<Value> {
    let (status, body) = http_request(host, port, Method::GET, path, None).await?;
    if !status.is_success() {
        return Err(Error::Http(format!("GET {} returned {}", path, status)));
    }
    let value: Value = serde_json::from_slice(&body)?;
    Ok(value)
}

/// HTTP POST with JSON body, returning parsed JSON.
pub async fn post_json(host: &str, port: u16, path: &str, body: &Value) -> Result<Value> {
    let (_status, resp) = http_request(host, port, Method::POST, path, Some(body)).await?;
    let value: Value = serde_json::from_slice(&resp)?;
    Ok(value)
}

/// HTTP DELETE, returning parsed JSON.
pub async fn delete_json(host: &str, port: u16, path: &str) -> Result<Value> {
    let (_status, resp) = http_request(host, port, Method::DELETE, path, None).await?;
    if resp.is_empty() {
        return Ok(Value::Null);
    }
    let value: Value = serde_json::from_slice(&resp)?;
    Ok(value)
}
