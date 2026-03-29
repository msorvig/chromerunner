//! Firefox automation via WebDriver BiDi.
//!
//! Requires [`geckodriver`](https://github.com/mozilla/geckodriver) on
//! `$PATH`. The driver is launched automatically and a WebDriver session is
//! created with `webSocketUrl: true` to enable BiDi. All page-level
//! commands (navigate, evaluate, etc.) go over the BiDi WebSocket;
//! `set_bounds` uses geckodriver's WebDriver REST endpoint alongside BiDi.
//!
//! The underlying [`BidiClient`] is exposed via
//! [`Browser::bidi()`] for sending arbitrary BiDi commands.

pub mod bidi;

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::launcher;
use crate::transport::http;
use crate::types::{BrowserApi, JsResult, TabApi, TargetInfo};
use bidi::BidiClient;

/// Firefox browser instance (via geckodriver + WebDriver BiDi).
pub struct Browser {
    bidi: Arc<BidiClient>,
    session_id: String,
    driver_port: u16,
    _driver: Option<launcher::ChildGuard>,
}

/// A Firefox browsing context (tab or window).
pub struct Tab {
    context_id: String,
    bidi: Arc<BidiClient>,
    session_id: String,
    driver_port: u16,
    on_navigate_scripts: std::sync::Mutex<Vec<String>>,
}

impl Browser {
    /// Launch Firefox via geckodriver. Requires `geckodriver` on PATH.
    pub async fn launch(headless: bool) -> Result<Self> {
        info!(headless, "Firefox: launching");
        let port = launcher::find_free_port()?;
        let ws_port = launcher::find_free_port()?;
        let driver = launcher::launch_geckodriver(port, ws_port, headless)?;

        // Wait for geckodriver to be ready
        wait_for_driver(port).await?;

        // Let geckodriver manage the profile (it creates a temp one automatically).
        // Pass prefs to suppress keychain/telemetry/default-browser prompts.
        let mut ff_args: Vec<String> = Vec::new();
        if headless {
            ff_args.push("-headless".to_string());
        }

        let mut ff_options = json!({
            "prefs": {
                "signon.rememberSignons": false,
                "toolkit.telemetry.reportingpolicy.firstRun": false,
                "datareporting.policy.dataSubmissionEnabled": false,
                "browser.shell.checkDefaultBrowser": false,
                "browser.startup.homepage_override.mstone": "ignore",
            }
        });
        if !ff_args.is_empty() {
            ff_options["args"] = json!(ff_args);
        }

        let caps = json!({
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "firefox",
                    "webSocketUrl": true,
                    "moz:firefoxOptions": ff_options,
                }
            }
        });

        let resp = http::post_json("127.0.0.1", port, "/session", &caps).await?;

        // Check for error response from geckodriver
        if let Some(err) = resp.get("value").and_then(|v| v.get("error")) {
            let message = resp["value"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            return Err(Error::LaunchFailed(format!(
                "geckodriver session error: {}: {}",
                err, message
            )));
        }

        let session_id = resp["value"]["sessionId"]
            .as_str()
            .ok_or_else(|| {
                Error::ConnectionFailed(format!(
                    "no sessionId in response: {}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                ))
            })?
            .to_string();
        let ws_url = resp["value"]["capabilities"]["webSocketUrl"]
            .as_str()
            .ok_or_else(|| Error::ConnectionFailed("no webSocketUrl in capabilities".into()))?
            .to_string();

        let bidi = BidiClient::connect(&ws_url).await?;
        info!("Firefox: ready");

        Ok(Self {
            bidi: Arc::new(bidi),
            session_id,
            driver_port: port,
            _driver: Some(driver),
        })
    }

    pub async fn version(&self) -> Result<Value> {
        let resp =
            http::get_json("127.0.0.1", self.driver_port, "/status").await?;
        Ok(resp["value"].clone())
    }

    pub async fn new_tab(&self, url: &str) -> Result<Tab> {
        self.create_context("tab", url).await
    }

    pub async fn new_window(&self, url: &str) -> Result<Tab> {
        self.create_context("window", url).await
    }

    pub async fn list_targets(&self) -> Result<Vec<TargetInfo>> {
        let result = self
            .bidi
            .send("browsingContext.getTree", json!({}))
            .await?;
        let contexts = result["contexts"].as_array().cloned().unwrap_or_default();
        Ok(contexts
            .iter()
            .map(|c| TargetInfo {
                target_id: c["context"].as_str().unwrap_or("").to_string(),
                title: String::new(), // BiDi getTree doesn't include title
                url: c["url"].as_str().unwrap_or("").to_string(),
                target_type: c["type"].as_str().unwrap_or("").to_string(),
            })
            .collect())
    }

    pub async fn close(mut self) -> Result<()> {
        // Delete the WebDriver session (this closes the browser)
        let _ = http::delete_json(
            "127.0.0.1",
            self.driver_port,
            &format!("/session/{}", self.session_id),
        )
        .await;
        if let Some(mut d) = self._driver.take() {
            d.kill();
        }
        Ok(())
    }

    /// Access the underlying BiDi client.
    pub fn bidi(&self) -> &Arc<BidiClient> {
        &self.bidi
    }

    async fn create_context(&self, ctx_type: &str, url: &str) -> Result<Tab> {
        let result = self
            .bidi
            .send(
                "browsingContext.create",
                json!({ "type": ctx_type }),
            )
            .await?;
        let context_id = result["context"]
            .as_str()
            .ok_or_else(|| Error::Other("no context in response".into()))?
            .to_string();

        let tab = Tab {
            context_id,
            bidi: self.bidi.clone(),
            session_id: self.session_id.clone(),
            driver_port: self.driver_port,
            on_navigate_scripts: std::sync::Mutex::new(Vec::new()),
        };

        if url != "about:blank" {
            tab.navigate(url).await?;
        }

        Ok(tab)
    }
}

impl Tab {
    pub fn target_id(&self) -> &str {
        &self.context_id
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        debug!(url, context_id = self.context_id, "Firefox: navigate");
        self.bidi
            .send(
                "browsingContext.navigate",
                json!({
                    "context": self.context_id,
                    "url": url,
                    "wait": "complete",
                }),
            )
            .await?;
        self.run_on_navigate_scripts().await;
        Ok(())
    }

    pub async fn evaluate(&self, expression: &str) -> Result<JsResult> {
        let result = self
            .bidi
            .send(
                "script.evaluate",
                json!({
                    "expression": expression,
                    "target": { "context": self.context_id },
                    "awaitPromise": true,
                    "resultOwnership": "none",
                    "serializationOptions": {
                        "maxDomDepth": 0,
                        "maxObjectDepth": 10,
                    },
                }),
            )
            .await?;

        // Check for exceptions
        if result.get("exceptionDetails").is_some() {
            let text = result["exceptionDetails"]["text"]
                .as_str()
                .or_else(|| {
                    result["exceptionDetails"]["exception"]["value"]
                        .as_str()
                })
                .unwrap_or("unknown error");
            return Err(Error::JavaScript(text.to_string()));
        }

        let remote = &result["result"];
        let result_type = remote["type"].as_str().unwrap_or("undefined").to_string();
        let value = bidi_to_json(remote);

        Ok(JsResult { value, result_type })
    }

    pub async fn inject_preload_script(&self, source: &str) -> Result<()> {
        self.bidi
            .send(
                "script.addPreloadScript",
                json!({
                    "functionDeclaration": format!("() => {{ {} }}", source),
                }),
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

    async fn run_on_navigate_scripts(&self) {
        let scripts = self.on_navigate_scripts.lock().unwrap().clone();
        for script in &scripts {
            let _ = self.evaluate(script).await;
        }
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
        // BiDi has no window-bounds command, but geckodriver exposes the
        // WebDriver REST endpoint alongside BiDi on the same port.
        http::post_json(
            "127.0.0.1",
            self.driver_port,
            &format!("/session/{}/window/rect", self.session_id),
            &json!({ "x": x, "y": y, "width": width, "height": height }),
        )
        .await?;
        Ok(())
    }

    pub async fn close(self) -> Result<()> {
        self.bidi
            .send(
                "browsingContext.close",
                json!({ "context": self.context_id }),
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

/// Convert a BiDi remote value to a serde_json::Value.
fn bidi_to_json(remote: &Value) -> Value {
    match remote["type"].as_str() {
        Some("string") => remote["value"].clone(),
        Some("number") => {
            if let Some(n) = remote["value"].as_f64() {
                // Preserve integer representation when possible
                if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
                    json!(n as i64)
                } else {
                    json!(n)
                }
            } else {
                Value::Null
            }
        }
        Some("boolean") => remote["value"].clone(),
        Some("null") | Some("undefined") => Value::Null,
        Some("array") => {
            let items = remote["value"].as_array().cloned().unwrap_or_default();
            Value::Array(items.iter().map(bidi_to_json).collect())
        }
        Some("object") => {
            let entries = remote["value"].as_array().cloned().unwrap_or_default();
            let mut obj = serde_json::Map::new();
            for entry in &entries {
                if let Some(pair) = entry.as_array() {
                    if pair.len() == 2 {
                        let key = pair[0].as_str().unwrap_or("").to_string();
                        obj.insert(key, bidi_to_json(&pair[1]));
                    }
                }
            }
            Value::Object(obj)
        }
        _ => Value::Null,
    }
}

async fn wait_for_driver(port: u16) -> Result<()> {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if http::get_json("127.0.0.1", port, "/status").await.is_ok() {
            return Ok(());
        }
    }
    Err(Error::Timeout("geckodriver did not start within 5s".into()))
}
