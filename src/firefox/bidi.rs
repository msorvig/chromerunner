//! WebDriver BiDi WebSocket message routing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use fastwebsockets::{Frame, OpCode};
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{debug, trace, warn};

use crate::error::{Error, Result};
use crate::transport::ws::{self, WsWriter};

/// A BiDi event pushed by the browser.
#[derive(Clone, Debug)]
pub struct BidiEvent {
    pub method: String,
    pub params: Value,
}

type PendingResult = std::result::Result<Value, Error>;

struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<PendingResult>>>,
    events: broadcast::Sender<BidiEvent>,
}

/// Low-level WebDriver BiDi client over WebSocket.
pub struct BidiClient {
    shared: Arc<Shared>,
    writer: Mutex<WsWriter>,
    next_id: AtomicU64,
    _read_task: tokio::task::JoinHandle<()>,
}

impl BidiClient {
    /// Connect to a BiDi WebSocket endpoint.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        debug!(url = ws_url, "BiDi connecting");
        let (read_half, write_half) = ws::ws_connect(ws_url).await?;

        let (event_tx, _) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            events: event_tx,
        });

        let shared2 = shared.clone();
        let read_task = tokio::spawn(async move {
            Self::read_loop(read_half, shared2).await;
        });

        debug!("BiDi connected");
        Ok(Self {
            shared,
            writer: Mutex::new(write_half),
            next_id: AtomicU64::new(1),
            _read_task: read_task,
        })
    }

    /// Send a BiDi command and wait for the response.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        self.shared.pending.lock().await.insert(id, tx);

        let msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        debug!(id, method, "BiDi send");
        trace!(message = %msg, "BiDi send raw");

        let text = serde_json::to_string(&msg)?;
        let frame = Frame::text(fastwebsockets::Payload::Owned(text.into_bytes()));
        self.writer
            .lock()
            .await
            .write_frame(frame)
            .await
            .map_err(|e| Error::WebSocket(e.to_string()))?;

        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(result)) => {
                debug!(id, method, "BiDi response ok");
                let value = result?;
                trace!(id, result = %value, "BiDi response raw");
                Ok(value)
            }
            Ok(Err(_)) => Err(Error::Other("response channel closed".into())),
            Err(_) => {
                warn!(id, method, "BiDi command timed out");
                Err(Error::Timeout("BiDi command timed out after 30s".into()))
            }
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<BidiEvent> {
        self.shared.events.subscribe()
    }

    async fn read_loop(mut read: ws::WsReader, shared: Arc<Shared>) {
        let mut noop =
            |_: Frame<'_>| std::future::ready(Ok::<(), fastwebsockets::WebSocketError>(()));

        loop {
            let frame = match read.read_frame(&mut noop).await {
                Ok(f) => f,
                Err(e) => {
                    debug!(error = %e, "BiDi read loop ended");
                    break;
                }
            };

            match frame.opcode {
                OpCode::Text => {
                    let value: Value = match serde_json::from_slice(&frame.payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let msg_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    match msg_type {
                        "success" => {
                            if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                                trace!(id, "BiDi received success");
                                let mut pending = shared.pending.lock().await;
                                if let Some(sender) = pending.remove(&id) {
                                    let result =
                                        value.get("result").cloned().unwrap_or(Value::Null);
                                    let _ = sender.send(Ok(result));
                                }
                            }
                        }
                        "error" => {
                            if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                                let mut pending = shared.pending.lock().await;
                                if let Some(sender) = pending.remove(&id) {
                                    let error_type = value
                                        .get("error")
                                        .and_then(|e| e.as_str())
                                        .unwrap_or("unknown");
                                    let message = value
                                        .get("message")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("unknown error")
                                        .to_string();
                                    debug!(id, error_type, %message, "BiDi error response");
                                    let _ = sender.send(Err(Error::Protocol {
                                        code: 0,
                                        message: format!("{}: {}", error_type, message),
                                        data: None,
                                    }));
                                }
                            }
                        }
                        "event" => {
                            if let Some(method) =
                                value.get("method").and_then(|m| m.as_str())
                            {
                                debug!(method, "BiDi event");
                                trace!(event = %value, "BiDi event raw");
                                let event = BidiEvent {
                                    method: method.to_string(),
                                    params: value
                                        .get("params")
                                        .cloned()
                                        .unwrap_or(Value::Null),
                                };
                                let _ = shared.events.send(event);
                            }
                        }
                        _ => {}
                    }
                }
                OpCode::Close => {
                    debug!("BiDi WebSocket closed by server");
                    break;
                }
                _ => {}
            }
        }
    }
}
