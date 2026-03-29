//! Shared WebSocket connect + split (used by Chrome CDP and Firefox BiDi).

use bytes::Bytes;
use fastwebsockets::handshake::generate_key;
use fastwebsockets::{FragmentCollectorRead, Role, WebSocket};
use http_body_util::Empty;
use hyper::header::{CONNECTION, UPGRADE};
use hyper::upgrade::Upgraded;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::error::{Error, Result};

/// Read half of a split WebSocket connection.
pub type WsReader = FragmentCollectorRead<tokio::io::ReadHalf<TokioIo<Upgraded>>>;
/// Write half of a split WebSocket connection.
pub type WsWriter = fastwebsockets::WebSocketWrite<tokio::io::WriteHalf<TokioIo<Upgraded>>>;

/// Perform a WebSocket handshake to `ws_url` and return split read/write halves.
pub async fn ws_connect(ws_url: &str) -> Result<(WsReader, WsWriter)> {
    info!(url = ws_url, "WebSocket connecting");

    let uri: hyper::Uri = ws_url
        .parse()
        .map_err(|e: hyper::http::uri::InvalidUri| Error::ConnectionFailed(e.to_string()))?;
    let host = uri.host().unwrap_or("127.0.0.1");
    let port = uri.port_u16().unwrap_or(9222);
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");

    debug!(host, port, path, "WebSocket TCP connect");

    let stream = TcpStream::connect(format!("{}:{}", host, port))
        .await
        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;

    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;

    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });

    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", format!("{}:{}", host, port))
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "upgrade")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .body(Empty::<Bytes>::new())
        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;

    let response = sender
        .send_request(req)
        .await
        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;

    debug!(status = %response.status(), "WebSocket upgrade response");

    let upgraded = hyper::upgrade::on(response)
        .await
        .map_err(|e| Error::ConnectionFailed(e.to_string()))?;

    let mut ws = WebSocket::after_handshake(TokioIo::new(upgraded), Role::Client);
    ws.set_auto_close(false);
    ws.set_auto_pong(false);

    let (read, write) = ws.split(tokio::io::split);
    let read = FragmentCollectorRead::new(read);

    info!("WebSocket connected");
    Ok((read, write))
}
