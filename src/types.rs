//! Shared types and traits used across all browser backends.

use serde_json::Value;

use crate::error::Result;

/// The result of evaluating a JavaScript expression.
#[derive(Debug, Clone)]
pub struct JsResult {
    /// The returned value, serialised to JSON.
    ///
    /// Primitive values map directly to their JSON equivalents. Objects and
    /// arrays are fully serialised. `undefined` becomes `Value::Null`.
    pub value: Value,

    /// The JavaScript type string: `"string"`, `"number"`, `"boolean"`,
    /// `"object"`, `"undefined"`, `"function"`, etc.
    pub result_type: String,
}

/// Metadata about a browser target / browsing context.
#[derive(Debug, Clone)]
pub struct TargetInfo {
    /// Unique identifier for this target.
    pub target_id: String,
    /// Page title (may be empty).
    pub title: String,
    /// Current URL of the target.
    pub url: String,
    /// Target type: `"page"`, `"tab"`, `"window"`, etc.
    pub target_type: String,
}

/// Browser-level operations shared by all backends.
///
/// Each backend ([`chrome::Browser`](crate::chrome::Browser),
/// [`firefox::Browser`](crate::firefox::Browser),
/// [`safari::Browser`](crate::safari::Browser)) implements this trait,
/// allowing generic code that works across browsers:
///
/// ```rust,no_run
/// use chromerunner::{BrowserApi, TabApi};
///
/// async fn get_title(browser: &impl BrowserApi) -> chromerunner::Result<String> {
///     let tab = browser.new_tab("https://example.com").await?;
///     let title = tab.title().await?;
///     tab.close().await?;
///     Ok(title)
/// }
/// ```
///
/// The concrete browser type is resolved at compile time (monomorphization).
/// There is no `dyn BrowserApi` support — use a `match` to dispatch at
/// runtime if needed.
pub trait BrowserApi: Sized {
    /// The tab type returned by this browser.
    type Tab: TabApi;

    /// Launch a new browser instance.
    ///
    /// If `headless` is `true`, the browser runs without a visible window
    /// (not supported by Safari — the parameter is accepted but ignored).
    fn launch(
        headless: bool,
    ) -> impl std::future::Future<Output = Result<Self>> + Send;

    /// Open a new tab and navigate to `url`.
    fn new_tab(
        &self,
        url: &str,
    ) -> impl std::future::Future<Output = Result<Self::Tab>> + Send;

    /// Open a new window and navigate to `url`.
    fn new_window(
        &self,
        url: &str,
    ) -> impl std::future::Future<Output = Result<Self::Tab>> + Send;

    /// List all targets / browsing contexts.
    fn list_targets(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<TargetInfo>>> + Send;

    /// Get browser version information.
    fn version(&self) -> impl std::future::Future<Output = Result<Value>> + Send;

    /// Gracefully close the browser and clean up resources.
    fn close(self) -> impl std::future::Future<Output = Result<()>> + Send;
}

/// Tab / window operations shared by all backends.
///
/// See [`BrowserApi`] for usage examples.
pub trait TabApi: Sized {
    /// Unique identifier for this tab / browsing context.
    fn target_id(&self) -> &str;

    /// Navigate to `url` and wait for the page to load.
    fn navigate(
        &self,
        url: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Evaluate a JavaScript expression and return the result.
    ///
    /// Promises are automatically awaited. On Safari the expression is
    /// wrapped in `return (...)` automatically.
    fn evaluate(
        &self,
        expression: &str,
    ) -> impl std::future::Future<Output = Result<JsResult>> + Send;

    /// Register a preload script that runs *before* page scripts on every
    /// new document load.
    ///
    /// Supported on Chrome (CDP `Page.addScriptToEvaluateOnNewDocument`)
    /// and Firefox (BiDi `script.addPreloadScript`). Returns an error on
    /// Safari where this capability does not exist.
    fn inject_preload_script(
        &self,
        source: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Register a script that runs *after* each [`navigate`](Self::navigate)
    /// call completes (after page load).
    ///
    /// Works consistently on all browsers. Useful for instrumentation,
    /// setting globals, or reading page state after every navigation.
    fn inject_on_navigate(
        &self,
        source: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Get the current page URL.
    fn url(&self) -> impl std::future::Future<Output = Result<String>> + Send;

    /// Get the current page title.
    fn title(&self) -> impl std::future::Future<Output = Result<String>> + Send;

    /// Set the window position and size.
    fn set_bounds(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Close this tab or window.
    fn close(self) -> impl std::future::Future<Output = Result<()>> + Send;
}
