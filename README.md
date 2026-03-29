# chromerunner

Multi-browser automation for Chrome, Firefox, and Safari from Rust.

Each browser is driven via its native automation protocol:

| Feature   | Browser           | Protocol           | Transport  |
|-----------|-------------------|--------------------|------------|
| `chrome`  | Chrome / Chromium | Chrome DevTools Protocol (CDP) | WebSocket |
| `firefox` | Firefox           | WebDriver BiDi     | WebSocket  |
| `safari`  | Safari            | WebDriver Classic  | HTTP REST  |

## Quick start

### Pick a browser

```rust
// Chrome (default feature, just works)
let browser = chromerunner::chrome::Browser::launch(true).await?;

// Firefox (needs geckodriver on $PATH)
let browser = chromerunner::firefox::Browser::launch(true).await?;

// Safari (needs one-time `safaridriver --enable`; no headless mode)
let browser = chromerunner::safari::Browser::launch(false).await?;
```

### Open windows and evaluate JavaScript

The API is the same across all browsers:

```rust
use chromerunner::chrome; // or firefox, or safari

let browser = chrome::Browser::launch(true).await?;

// Open tabs and windows (all share the same browser context)
let tab1 = browser.new_tab("https://example.com").await?;
let tab2 = browser.new_tab("about:blank").await?;
let window = browser.new_window("about:blank").await?;

// Navigate, get page info
tab2.navigate("https://example.com").await?;
println!("Title: {}", tab1.title().await?);
println!("URL:   {}", tab1.url().await?);

// Evaluate JavaScript — expressions, DOM, promises
let r = tab1.evaluate("2 + 2").await?;
println!("{} ({})", r.value, r.result_type);  // 4 (number)

let r = tab1.evaluate("document.title").await?;
println!("{}", r.value);

let r = tab1.evaluate("fetch('/api').then(r => r.status)").await?;
println!("status: {}", r.value);  // promises are awaited automatically

// Position windows
window.set_bounds(0, 0, 800, 600).await?;

// Inject a script that runs after every navigate (all browsers)
tab1.inject_on_navigate("window.__ready = true").await?;
tab1.navigate("https://example.com").await?;

// Clean up
tab1.close().await?;
tab2.close().await?;
window.close().await?;
browser.close().await?;
```

### Cross-browser generic code

Use `BrowserApi` and `TabApi` to write functions that work with any backend:

```rust
use chromerunner::{BrowserApi, TabApi};

async fn scrape(browser: &impl BrowserApi) -> chromerunner::Result<String> {
    let tab = browser.new_tab("https://example.com").await?;
    let title = tab.title().await?;
    tab.close().await?;
    Ok(title)
}
```

## CLI

```
chromerunner --browser chrome|firefox|safari [--headless] [--trace] <command>

Commands:
  version    Print browser version info
  navigate   Navigate to a URL and print page title
  eval       Evaluate a JavaScript expression
  targets    List all browser targets
  demo       Run a demo exercising major features (--pause to wait)
```

## API surface

All three backends implement `BrowserApi` and `TabApi`:

| Method | Chrome | Firefox | Safari | Notes |
|--------|:------:|:-------:|:------:|-------|
| `Browser::launch(headless)` | Yes | Yes | Yes | Safari ignores `headless` |
| `Browser::launch_with_args(headless, args)` | Yes | Yes | Yes | Safari warns and ignores args |
| `Browser::new_tab(url)` | Yes | Yes | Yes | |
| `Browser::new_window(url)` | Yes | Yes | Yes | |
| `Browser::list_targets()` | Yes | Yes | Yes | Safari returns handles only |
| `Browser::version()` | Yes | Yes | Yes | |
| `Browser::close()` | Yes | Yes | Yes | |
| `Tab::navigate(url)` | Yes | Yes | Yes | |
| `Tab::evaluate(expr)` | Yes | Yes | Yes | Safari wraps in `return(...)` |
| `Tab::inject_preload_script(src)` | Yes | Yes | Error | Before page scripts |
| `Tab::inject_on_navigate(src)` | Yes | Yes | Yes | After page load |
| `Tab::url()` | Yes | Yes | Yes | |
| `Tab::title()` | Yes | Yes | Yes | |
| `Tab::set_bounds(x,y,w,h)` | Yes | Yes | Yes | |
| `Tab::close()` | Yes | Yes | Yes | |

## Browser differences

### Headless mode

- **Chrome**: Full support (`--headless=new`).
- **Firefox**: Full support (`-headless`).
- **Safari**: No headless mode. A visible window always opens.

### Script injection

- `inject_preload_script` — runs *before* page scripts. Chrome and Firefox only.
- `inject_on_navigate` — runs *after* each `navigate()`. All browsers.

### evaluate

- Chrome / Firefox: expression evaluated directly.
- Safari: wrapped in `return(...)` automatically. Multi-statement code uses an IIFE.

### Prerequisites

- **Chrome**: just needs Chrome installed.
- **Firefox**: needs [`geckodriver`](https://github.com/mozilla/geckodriver) on `$PATH`. Firefox must not already be running (geckodriver uses `-no-remote`).
- **Safari**: needs a one-time `safaridriver --enable` (password prompt). Only one session at a time.

## Cargo features

```toml
[features]
default = ["chrome", "firefox", "safari", "cli"]
chrome  = ["dep:fastwebsockets"]   # CDP over WebSocket
firefox = ["dep:fastwebsockets"]   # BiDi over WebSocket
safari  = []                       # WebDriver REST — zero extra deps
cli     = ["dep:clap", "dep:anyhow", "dep:tracing-subscriber"]
```

Library users can disable features they don't need:

```toml
chromerunner = { version = "0.2", default-features = false, features = ["chrome"] }
```

## Tracing

The library is instrumented with `tracing`. The CLI enables output via `--trace` / `-t`:

- `--trace` — debug-level output (operations, lifecycle events)
- `RUST_LOG=chromerunner=trace --trace` — full protocol message bodies
