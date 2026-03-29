//! Chrome / Chromium automation via the Chrome DevTools Protocol (CDP).
//!
//! This is the most feature-complete backend. Chrome is launched with
//! `--remote-debugging-port`, and all communication happens over a single
//! WebSocket connection using CDP's JSON-RPC messages.
//!
//! The underlying [`CdpClient`] is exposed via
//! [`Browser::cdp()`] for sending arbitrary CDP commands beyond what the
//! high-level API provides.

pub mod cdp;

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::launcher;
use crate::transport::http;
use crate::types::{BrowserApi, JsResult, TabApi, TargetInfo};
use cdp::CdpClient;

/// Chrome browser instance.
pub struct Browser {
    cdp: Arc<CdpClient>,
    process: Option<launcher::ChildGuard>,
}

/// A Chrome tab or window.
pub struct Tab {
    target_id: String,
    session_id: String,
    cdp: Arc<CdpClient>,
    on_navigate_scripts: std::sync::Mutex<Vec<String>>,
}

impl Browser {
    /// Launch a new Chrome instance.
    pub async fn launch(headless: bool) -> Result<Self> {
        info!(headless, "Chrome: launching");
        let port = launcher::find_free_port()?;
        let process = launcher::launch_chrome(port, headless)?;
        let ws_url = wait_for_chrome(port).await?;
        let cdp = CdpClient::connect(&ws_url).await?;
        info!("Chrome: ready");

        Ok(Self {
            cdp: Arc::new(cdp),
            process: Some(process),
        })
    }

    /// Connect to an already-running Chrome instance.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let cdp = CdpClient::connect(ws_url).await?;
        Ok(Self {
            cdp: Arc::new(cdp),
            process: None,
        })
    }

    pub async fn version(&self) -> Result<Value> {
        self.cdp.send("Browser.getVersion", json!({}), None).await
    }

    pub async fn new_tab(&self, url: &str) -> Result<Tab> {
        self.create_target(url, false).await
    }

    pub async fn new_window(&self, url: &str) -> Result<Tab> {
        self.create_target(url, true).await
    }

    pub async fn list_targets(&self) -> Result<Vec<TargetInfo>> {
        let result = self.cdp.send("Target.getTargets", json!({}), None).await?;
        let targets = result["targetInfos"]
            .as_array()
            .ok_or_else(|| Error::Other("no targetInfos".into()))?;
        Ok(targets
            .iter()
            .map(|t| TargetInfo {
                target_id: t["targetId"].as_str().unwrap_or("").to_string(),
                title: t["title"].as_str().unwrap_or("").to_string(),
                url: t["url"].as_str().unwrap_or("").to_string(),
                target_type: t["type"].as_str().unwrap_or("").to_string(),
            })
            .collect())
    }

    pub async fn close(mut self) -> Result<()> {
        let _ = self.cdp.send("Browser.close", json!({}), None).await;
        if let Some(mut p) = self.process.take() {
            p.kill();
        }
        Ok(())
    }

    /// Access the underlying CDP client.
    pub fn cdp(&self) -> &Arc<CdpClient> {
        &self.cdp
    }

    async fn create_target(&self, url: &str, new_window: bool) -> Result<Tab> {
        let mut params = json!({ "url": url });
        if new_window {
            params["newWindow"] = json!(true);
        }
        let result = self.cdp.send("Target.createTarget", params, None).await?;
        let target_id = result["targetId"]
            .as_str()
            .ok_or_else(|| Error::Other("no targetId".into()))?
            .to_string();

        let attach = self
            .cdp
            .send(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
            )
            .await?;
        let session_id = attach["sessionId"]
            .as_str()
            .ok_or_else(|| Error::Other("no sessionId".into()))?
            .to_string();

        let tab = Tab {
            target_id,
            session_id,
            cdp: self.cdp.clone(),
            on_navigate_scripts: std::sync::Mutex::new(Vec::new()),
        };
        tab.wait_for_load().await?;
        Ok(tab)
    }
}

impl Tab {
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        debug!(url, target_id = self.target_id, "Chrome: navigate");
        self.cdp
            .send("Page.enable", json!({}), Some(&self.session_id))
            .await?;
        self.cdp
            .send(
                "Page.navigate",
                json!({ "url": url }),
                Some(&self.session_id),
            )
            .await?;
        self.wait_for_load().await?;
        self.run_on_navigate_scripts().await;
        Ok(())
    }

    pub async fn evaluate(&self, expression: &str) -> Result<JsResult> {
        debug!(target_id = self.target_id, expression, "Chrome: evaluate");
        let result = self
            .cdp
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
                Some(&self.session_id),
            )
            .await?;

        if let Some(exception) = result.get("exceptionDetails") {
            let text = exception
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .or_else(|| exception.get("text").and_then(|t| t.as_str()))
                .unwrap_or("unknown error");
            return Err(Error::JavaScript(text.to_string()));
        }

        let ro = &result["result"];
        Ok(JsResult {
            value: ro.get("value").cloned().unwrap_or(Value::Null),
            result_type: ro
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("undefined")
                .to_string(),
        })
    }

    pub async fn inject_preload_script(&self, source: &str) -> Result<()> {
        self.cdp
            .send(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": source }),
                Some(&self.session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn inject_on_navigate(&self, source: &str) -> Result<()> {
        self.on_navigate_scripts
            .lock()
            .unwrap()
            .push(source.to_string());
        Ok(())
    }

    pub async fn url(&self) -> Result<String> {
        let r = self.evaluate("location.href").await?;
        Ok(r.value.as_str().unwrap_or("").to_string())
    }

    pub async fn title(&self) -> Result<String> {
        let r = self.evaluate("document.title").await?;
        Ok(r.value.as_str().unwrap_or("").to_string())
    }

    pub async fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) -> Result<()> {
        let win = self
            .cdp
            .send(
                "Browser.getWindowForTarget",
                json!({ "targetId": self.target_id }),
                None,
            )
            .await?;
        let wid = win["windowId"]
            .as_i64()
            .ok_or_else(|| Error::Other("no windowId".into()))?;
        self.cdp
            .send(
                "Browser.setWindowBounds",
                json!({ "windowId": wid, "bounds": { "windowState": "normal" } }),
                None,
            )
            .await?;
        self.cdp
            .send(
                "Browser.setWindowBounds",
                json!({ "windowId": wid, "bounds": { "left": x, "top": y, "width": width, "height": height } }),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.cdp
            .send(
                "Target.closeTarget",
                json!({ "targetId": self.target_id }),
                None,
            )
            .await?;
        Ok(())
    }

    async fn run_on_navigate_scripts(&self) {
        let scripts = self.on_navigate_scripts.lock().unwrap().clone();
        for script in &scripts {
            let _ = self.evaluate(script).await;
        }
    }

    async fn wait_for_load(&self) -> Result<()> {
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if let Ok(r) = self
                .cdp
                .send(
                    "Runtime.evaluate",
                    json!({ "expression": "document.readyState", "returnByValue": true }),
                    Some(&self.session_id),
                )
                .await
            {
                if r.get("result")
                    .and_then(|r| r.get("value"))
                    .and_then(|v| v.as_str())
                    == Some("complete")
                {
                    return Ok(());
                }
            }
        }
        Err(Error::Timeout("page load timed out".into()))
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

async fn wait_for_chrome(port: u16) -> Result<String> {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Ok(json) = http::get_json("127.0.0.1", port, "/json/version").await {
            if let Some(url) = json["webSocketDebuggerUrl"].as_str() {
                return Ok(url.to_string());
            }
        }
    }
    Err(Error::Timeout("Chrome did not start within 5s".into()))
}
