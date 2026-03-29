//! Safari automation via WebDriver Classic (safaridriver).
//!
//! **Prerequisites:** run `safaridriver --enable` once (requires password
//! prompt) to allow remote automation. Safari has **no headless mode** — a
//! visible browser window always opens.
//!
//! All operations use HTTP REST calls to safaridriver. There is no
//! WebSocket connection. Safari WebDriver requires *switching* to a window
//! before operating on it; the [`Tab`] methods handle this automatically.
//!
//! ### Limitations vs Chrome / Firefox
//!
//! - **No headless mode.**
//! - **`inject_script` is emulated.** Scripts are re-executed after each
//!   `navigate()` call, running *after* page scripts (not before).
//! - **`evaluate` wraps expressions** in `return (...)` automatically.
//! - **`list_targets` returns handles only** — no title or URL without
//!   switching to each window.

use std::sync::Mutex;

use serde_json::{json, Value};

use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::launcher;
use crate::transport::http;
use crate::types::{BrowserApi, JsResult, TabApi, TargetInfo};

/// Safari browser instance (via safaridriver).
pub struct Browser {
    session_id: String,
    port: u16,
    #[allow(dead_code)]
    current_handle: String,
    _driver: Option<launcher::ChildGuard>,
}

/// A Safari window or tab.
///
/// Safari WebDriver requires switching to a window before operating on it,
/// so operations on a `Tab` automatically switch first.
pub struct Tab {
    handle: String,
    session_id: String,
    port: u16,
    /// Scripts registered via `inject_on_navigate`. Re-executed after each `navigate`.
    on_navigate_scripts: Mutex<Vec<String>>,
}

impl Browser {
    /// Launch Safari via safaridriver.
    ///
    /// Requires `safaridriver --enable` to have been run once (needs user auth).
    /// Safari does not support headless mode — `headless` is accepted but ignored.
    pub async fn launch(headless: bool) -> Result<Self> {
        let _ = headless; // Safari has no headless mode
        info!("Safari: launching (headless not supported)");
        let port = launcher::find_free_port()?;
        let driver = launcher::launch_safaridriver(port)?;

        wait_for_driver(port).await?;

        let resp = http::post_json(
            "127.0.0.1",
            port,
            "/session",
            &json!({
                "capabilities": {
                    "alwaysMatch": {
                        "browserName": "safari",
                    }
                }
            }),
        )
        .await?;

        let session_id = resp["value"]["sessionId"]
            .as_str()
            .ok_or_else(|| Error::ConnectionFailed("no sessionId".into()))?
            .to_string();

        // Get the initial window handle
        let handles_resp = http::get_json(
            "127.0.0.1",
            port,
            &format!("/session/{}/window/handles", session_id),
        )
        .await?;
        let initial_handle = handles_resp["value"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string();

        info!(session_id = %session_id, "Safari: ready");
        Ok(Self {
            session_id,
            port,
            current_handle: initial_handle,
            _driver: Some(driver),
        })
    }

    pub async fn version(&self) -> Result<Value> {
        let resp =
            http::get_json("127.0.0.1", self.port, "/status").await?;
        Ok(resp["value"].clone())
    }

    pub async fn new_tab(&self, url: &str) -> Result<Tab> {
        self.create_window("tab", url).await
    }

    pub async fn new_window(&self, url: &str) -> Result<Tab> {
        self.create_window("window", url).await
    }

    pub async fn list_targets(&self) -> Result<Vec<TargetInfo>> {
        let resp = http::get_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/window/handles", self.session_id),
        )
        .await?;
        let handles = resp["value"].as_array().cloned().unwrap_or_default();
        Ok(handles
            .iter()
            .map(|h| TargetInfo {
                target_id: h.as_str().unwrap_or("").to_string(),
                title: String::new(),
                url: String::new(),
                target_type: "window".to_string(),
            })
            .collect())
    }

    pub async fn close(mut self) -> Result<()> {
        let _ = http::delete_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}", self.session_id),
        )
        .await;
        if let Some(mut d) = self._driver.take() {
            d.kill();
        }
        Ok(())
    }

    async fn create_window(&self, win_type: &str, url: &str) -> Result<Tab> {
        let resp = http::post_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/window/new", self.session_id),
            &json!({ "type": win_type }),
        )
        .await?;

        let handle = resp["value"]["handle"]
            .as_str()
            .ok_or_else(|| Error::Other("no handle in response".into()))?
            .to_string();

        let tab = Tab {
            handle,
            session_id: self.session_id.clone(),
            port: self.port,
            on_navigate_scripts: Mutex::new(Vec::new()),
        };

        // Switch to the new window and navigate
        tab.switch_to().await?;
        if url != "about:blank" {
            tab.navigate(url).await?;
        }

        Ok(tab)
    }
}

impl Tab {
    pub fn target_id(&self) -> &str {
        &self.handle
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        debug!(url, handle = self.handle, "Safari: navigate");
        self.switch_to().await?;
        http::post_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/url", self.session_id),
            &json!({ "url": url }),
        )
        .await?;

        // Run on-navigate scripts after page load
        let scripts = self.on_navigate_scripts.lock().unwrap().clone();
        for script in &scripts {
            let _ = self.evaluate(script).await;
        }

        Ok(())
    }

    pub async fn evaluate(&self, expression: &str) -> Result<JsResult> {
        debug!(handle = self.handle, expression, "Safari: evaluate");
        self.switch_to().await?;

        // WebDriver requires `return ...`. For single expressions, `return (expr)`
        // works. For multi-statement code (contains `;`), wrap in an IIFE so the
        // statements execute and the last expression is returned.
        let script = if expression.contains(';') {
            format!("return (function(){{ {} }})()", expression)
        } else {
            format!("return ({})", expression)
        };

        let resp = http::post_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/execute/sync", self.session_id),
            &json!({
                "script": script,
                "args": [],
            }),
        )
        .await?;

        // WebDriver wraps errors in value.error
        if let Some(err) = resp["value"].get("error") {
            let message = resp["value"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            if err.as_str() == Some("javascript error") {
                return Err(Error::JavaScript(message.to_string()));
            }
            return Err(Error::Other(message.to_string()));
        }

        let value = resp["value"].clone();
        let result_type = match &value {
            Value::Null => "object".to_string(), // WebDriver maps null
            Value::Bool(_) => "boolean".to_string(),
            Value::Number(_) => "number".to_string(),
            Value::String(_) => "string".to_string(),
            Value::Array(_) => "object".to_string(),
            Value::Object(_) => "object".to_string(),
        };

        Ok(JsResult { value, result_type })
    }

    /// Not supported on Safari — returns an error.
    pub async fn inject_preload_script(&self, _source: &str) -> Result<()> {
        Err(Error::Other(
            "inject_preload_script not supported on Safari (no WebDriver equivalent)".into(),
        ))
    }

    /// Register a script to run after each [`navigate`](Self::navigate) call.
    pub async fn inject_on_navigate(&self, source: &str) -> Result<()> {
        self.on_navigate_scripts
            .lock()
            .unwrap()
            .push(source.to_string());
        Ok(())
    }

    pub async fn url(&self) -> Result<String> {
        self.switch_to().await?;
        let resp = http::get_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/url", self.session_id),
        )
        .await?;
        Ok(resp["value"].as_str().unwrap_or("").to_string())
    }

    pub async fn title(&self) -> Result<String> {
        self.switch_to().await?;
        let resp = http::get_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/title", self.session_id),
        )
        .await?;
        Ok(resp["value"].as_str().unwrap_or("").to_string())
    }

    pub async fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
        self.switch_to().await?;
        http::post_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/window/rect", self.session_id),
            &json!({ "x": x, "y": y, "width": width, "height": height }),
        )
        .await?;
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.switch_to().await?;
        http::delete_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/window", self.session_id),
        )
        .await?;
        Ok(())
    }

    async fn switch_to(&self) -> Result<()> {
        http::post_json(
            "127.0.0.1",
            self.port,
            &format!("/session/{}/window", self.session_id),
            &json!({ "handle": self.handle }),
        )
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl BrowserApi for Browser {
    type Tab = Tab;

    async fn launch(headless: bool) -> Result<Self> { Self::launch(headless).await }
    async fn new_tab(&self, url: &str) -> Result<Tab> { self.new_tab(url).await }
    async fn new_window(&self, url: &str) -> Result<Tab> { self.new_window(url).await }
    async fn list_targets(&self) -> Result<Vec<TargetInfo>> { self.list_targets().await }
    async fn version(&self) -> Result<Value> { self.version().await }
    async fn close(self) -> Result<()> { self.close().await }
}

impl TabApi for Tab {
    fn target_id(&self) -> &str { self.target_id() }
    async fn navigate(&self, url: &str) -> Result<()> { self.navigate(url).await }
    async fn evaluate(&self, expr: &str) -> Result<JsResult> { self.evaluate(expr).await }
    async fn inject_preload_script(&self, src: &str) -> Result<()> { self.inject_preload_script(src).await }
    async fn inject_on_navigate(&self, src: &str) -> Result<()> { self.inject_on_navigate(src).await }
    async fn url(&self) -> Result<String> { self.url().await }
    async fn title(&self) -> Result<String> { self.title().await }
    async fn set_bounds(&self, x: i32, y: i32, w: i32, h: i32) -> Result<()> { self.set_bounds(x, y, w, h).await }
    async fn close(self) -> Result<()> { self.close().await }
}

async fn wait_for_driver(port: u16) -> Result<()> {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if http::get_json("127.0.0.1", port, "/status").await.is_ok() {
            return Ok(());
        }
    }
    Err(Error::Timeout("safaridriver did not start within 5s".into()))
}
