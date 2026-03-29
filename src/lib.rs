//! # chromerunner
//!
//! Multi-browser automation for Chrome, Firefox, and Safari from Rust.
//!
//! Each browser is driven via its native automation protocol:
//!
//! | Feature   | Browser | Protocol           | Transport  |
//! |-----------|---------|--------------------|------------|
//! | `chrome`  | Chrome / Chromium | Chrome DevTools Protocol (CDP) | WebSocket |
//! | `firefox` | Firefox | WebDriver BiDi     | WebSocket  |
//! | `safari`  | Safari  | WebDriver Classic  | HTTP REST  |
//!
//! Enable the backends you need via cargo features. The default enables
//! `chrome` only; adding `firefox` or `safari` costs zero additional
//! dependencies (they share `hyper` and `fastwebsockets` already pulled in
//! by `chrome`).
//!
//! ## Quick start
//!
//! ### Pick a browser
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), chromerunner::Error> {
//! // Chrome (default feature, just works)
//! let browser = chromerunner::chrome::Browser::launch(true).await?;
//!
//! // Firefox (needs geckodriver on $PATH)
//! // let browser = chromerunner::firefox::Browser::launch(true).await?;
//!
//! // Safari (needs one-time `safaridriver --enable`; no headless mode)
//! // let browser = chromerunner::safari::Browser::launch(false).await?;
//! # browser.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Open windows and evaluate JavaScript
//!
//! The API is the same across all browsers — use [`BrowserApi`] and [`TabApi`]
//! to write generic code, or call methods directly on the concrete types.
//!
//! ```rust,no_run
//! use chromerunner::chrome; // or firefox, or safari
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), chromerunner::Error> {
//! let browser = chrome::Browser::launch(true).await?;
//!
//! // Open tabs and windows (all share the same browser context)
//! let tab1 = browser.new_tab("https://example.com").await?;
//! let tab2 = browser.new_tab("about:blank").await?;
//! let window = browser.new_window("about:blank").await?;
//!
//! // Navigate, get page info
//! tab2.navigate("https://example.com").await?;
//! println!("Title: {}", tab1.title().await?);
//! println!("URL:   {}", tab1.url().await?);
//!
//! // Evaluate JavaScript — expressions, DOM, promises
//! let r = tab1.evaluate("2 + 2").await?;
//! println!("{} ({})", r.value, r.result_type);  // 4 (number)
//!
//! let r = tab1.evaluate("document.title").await?;
//! println!("{}", r.value);
//!
//! let r = tab1.evaluate("fetch('/api').then(r => r.status)").await?;
//! println!("status: {}", r.value);  // promises are awaited automatically
//!
//! // Position windows
//! window.set_bounds(0, 0, 800, 600).await?;
//!
//! // Inject a script that runs after every navigate (all browsers)
//! tab1.inject_on_navigate("window.__ready = true").await?;
//! tab1.navigate("https://example.com").await?;
//! // window.__ready is now set
//!
//! // Clean up
//! tab1.close().await?;
//! tab2.close().await?;
//! window.close().await?;
//! browser.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Cross-browser generic code
//!
//! ```rust,no_run
//! use chromerunner::{BrowserApi, TabApi};
//!
//! async fn scrape(browser: &impl BrowserApi) -> chromerunner::Result<String> {
//!     let tab = browser.new_tab("https://example.com").await?;
//!     let title = tab.title().await?;
//!     tab.close().await?;
//!     Ok(title)
//! }
//! ```
//!
//! ## API surface
//!
//! All three backends implement [`BrowserApi`] and [`TabApi`]:
//!
//! | Method | Chrome | Firefox | Safari | Notes |
//! |--------|:------:|:-------:|:------:|-------|
//! | `Browser::launch(headless)` | Yes | Yes | Yes | Safari ignores `headless` |
//! | `Browser::new_tab(url)` | Yes | Yes | Yes | |
//! | `Browser::new_window(url)` | Yes | Yes | Yes | |
//! | `Browser::list_targets()` | Yes | Yes | Yes | Safari returns handles only (no title/url) |
//! | `Browser::version()` | Yes | Yes | Yes | |
//! | `Browser::close()` | Yes | Yes | Yes | |
//! | `Tab::navigate(url)` | Yes | Yes | Yes | |
//! | `Tab::evaluate(expr)` | Yes | Yes | Yes | Safari wraps in `return(...)` automatically |
//! | `Tab::inject_preload_script(src)` | Yes | Yes | Error | Before page scripts |
//! | `Tab::inject_on_navigate(src)` | Yes | Yes | Yes | After page load |
//! | `Tab::url()` | Yes | Yes | Yes | |
//! | `Tab::title()` | Yes | Yes | Yes | |
//! | `Tab::set_bounds(x,y,w,h)` | Yes | Yes | Yes | |
//! | `Tab::close()` | Yes | Yes | Yes | |
//!
//! ## Browser differences and limitations
//!
//! ### Headless mode
//!
//! - **Chrome**: Full headless support via `--headless=new`.
//! - **Firefox**: Full headless support via `-headless` flag.
//! - **Safari**: **No headless mode.** A visible Safari window always opens.
//!   The `headless` parameter is accepted but ignored.
//!
//! ### Script injection
//!
//! **`inject_preload_script`** runs *before* page scripts on every new
//! document load. Supported on Chrome (`Page.addScriptToEvaluateOnNewDocument`)
//! and Firefox (`script.addPreloadScript`). Returns an error on Safari.
//!
//! **`inject_on_navigate`** runs *after* each [`Tab::navigate()`] completes.
//! Works consistently on all three browsers. Use this for cross-browser code.
//!
//! ### `evaluate` expression syntax
//!
//! - **Chrome / Firefox**: The expression is evaluated directly.
//!   `tab.evaluate("2 + 2")` returns `4`.
//! - **Safari**: WebDriver's `execute/sync` requires a `return` statement.
//!   The library automatically wraps your expression:
//!   `"2 + 2"` becomes `"return (2 + 2)"`. This works for expressions but
//!   not for statements like `if (...) { ... }`.
//!
//! ### `list_targets`
//!
//! - **Chrome**: Returns full target info (id, title, url, type) for all
//!   targets including pages, service workers, and extensions.
//! - **Firefox**: Returns browsing contexts from `browsingContext.getTree`.
//!   Includes context id, url, and type but not the page title.
//! - **Safari**: Returns window handles only (ids). Title and URL are not
//!   available without switching to each window.
//!
//! ### `set_bounds`
//!
//! - **Chrome**: Uses `Browser.setWindowBounds` (CDP). Works in both headed
//!   and headless modes.
//! - **Firefox**: Uses `POST /session/{id}/window/rect` (WebDriver REST,
//!   served by geckodriver alongside BiDi).
//! - **Safari**: Uses `POST /session/{id}/window/rect` (WebDriver REST).
//!
//! ### Raw protocol access
//!
//! - **Chrome**: [`chrome::Browser::cdp()`] exposes the underlying
//!   [`CdpClient`] for sending arbitrary CDP commands.
//! - **Firefox**: [`firefox::Browser::bidi()`] exposes the underlying
//!   [`firefox::bidi::BidiClient`] for sending arbitrary BiDi commands.
//! - **Safari**: No raw protocol access — all operations go through the
//!   typed HTTP helper functions.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │                   Your application                   │
//! │          fn scrape(b: &impl BrowserApi) { }          │
//! ├──────────────────────────────────────────────────────┤
//! │          BrowserApi + TabApi  (shared traits)         │
//! ├────────────────┬─────────────────┬───────────────────┤
//! │ chrome::Browser│ firefox::Browser│ safari::Browser   │
//! │ chrome::Tab    │ firefox::Tab    │ safari::Tab       │
//! │ (CDP)          │ (BiDi)          │ (WebDriver REST)  │
//! ├────────────────┴─────────────────┴───────────────────┤
//! │        Shared types (JsResult, TargetInfo, Error)     │
//! ├──────────────────────┬───────────────────────────────┤
//! │  transport::ws       │  transport::http              │
//! │  (fastwebsockets)    │  (hyper)                      │
//! │  Chrome + Firefox    │  all three backends           │
//! └──────────────────────┴───────────────────────────────┘
//! ```

pub mod error;
pub mod launcher;
pub mod transport;
pub mod types;

#[cfg(feature = "chrome")]
pub mod chrome;
#[cfg(feature = "firefox")]
pub mod firefox;
#[cfg(feature = "safari")]
pub mod safari;

// Re-export shared types and traits at crate root
pub use error::{Error, Result};
pub use types::{BrowserApi, JsResult, TabApi, TargetInfo};

// Backward-compatible re-exports: chrome types at crate root
#[cfg(feature = "chrome")]
pub use chrome::cdp::{CdpClient, CdpEvent};
#[cfg(feature = "chrome")]
pub use chrome::{Browser, Tab};
