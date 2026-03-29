//! CDP WebSocket message routing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use fastwebsockets::{Frame, OpCode};
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot, Mutex};
use tracing::{debug, trace, warn};

use crate::error::{Error, Result};
use crate::transport::ws::{self, WsWriter};

/// A CDP event pushed by Chrome.
#[derive(Clone, Debug)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

type PendingResult = std::result::Result<Value, Error>;

struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<PendingResult>>>,
    events: broadcast::Sender<CdpEvent>,
}

/// Low-level CDP client over a single WebSocket connection.
pub struct CdpClient {
    shared: Arc<Shared>,
    writer: Mutex<WsWriter>,
    next_id: AtomicU64,
    _read_task: tokio::task::JoinHandle<()>,
}

impl CdpClient {
    /// Connect to a CDP WebSocket endpoint.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        debug!(url = ws_url, "CDP connecting");
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

        debug!("CDP connected");
        Ok(Self {
            shared,
            writer: Mutex::new(write_half),
            next_id: AtomicU64::new(1),
            _read_task: read_task,
        })
    }

    /// Send a CDP command and wait for the response (up to 30 s).
    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        self.shared.pending.lock().await.insert(id, tx);

        let mut msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            msg["sessionId"] = Value::String(sid.to_string());
        }

        debug!(id, method, "CDP send");
        trace!(message = %msg, "CDP send raw");

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
                debug!(id, method, "CDP response ok");
                let value = result?;
                trace!(id, result = %value, "CDP response raw");
                Ok(value)
            }
            Ok(Err(_)) => Err(Error::Other("response channel closed".into())),
            Err(_) => {
                warn!(id, method, "CDP command timed out");
                Err(Error::Timeout("CDP command timed out after 30s".into()))
            }
        }
    }

    /// Subscribe to CDP events.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.shared.events.subscribe()
    }

    async fn read_loop(mut read: ws::WsReader, shared: Arc<Shared>) {
        let mut noop =
            |_: Frame<'_>| std::future::ready(Ok::<(), fastwebsockets::WebSocketError>(()));

        loop {
            let frame = match read.read_frame(&mut noop).await {
                Ok(f) => f,
                Err(e) => {
                    debug!(error = %e, "CDP read loop ended");
                    break;
                }
            };

            match frame.opcode {
                OpCode::Text => {
                    let value: Value = match serde_json::from_slice(&frame.payload) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                        trace!(id, "CDP received response");
                        let mut pending = shared.pending.lock().await;
                        if let Some(sender) = pending.remove(&id) {
                            if let Some(error) = value.get("error") {
                                let code = error
                                    .get("code")
                                    .and_then(|c| c.as_i64())
                                    .unwrap_or(0);
                                let message = error
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let data = error.get("data").cloned();
                                debug!(id, code, %message, "CDP error response");
                                let _ = sender.send(Err(Error::Protocol {
                                    code,
                                    message,
                                    data,
                                }));
                            } else {
                                let result =
                                    value.get("result").cloned().unwrap_or(Value::Null);
                                let _ = sender.send(Ok(result));
                            }
                        }
                    } else if let Some(method) =
                        value.get("method").and_then(|m| m.as_str())
                    {
                        debug!(method, "CDP event");
                        trace!(event = %value, "CDP event raw");
                        let event = CdpEvent {
                            method: method.to_string(),
                            params: value
                                .get("params")
                                .cloned()
                                .unwrap_or(Value::Null),
                            session_id: value
                                .get("sessionId")
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string()),
                        };
                        let _ = shared.events.send(event);
                    }
                }
                OpCode::Close => {
                    debug!("CDP WebSocket closed by server");
                    break;
                }
                _ => {}
            }
        }
    }
}
