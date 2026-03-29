# Multi-Browser Architecture Plan

## Goal

Support Chrome, Firefox, and Safari behind cargo features with minimal
dependency overhead when all are enabled.

## Protocol Summary

| Browser | Protocol | Transport | Session Bootstrap |
|---------|----------|-----------|-------------------|
| Chrome  | CDP | WebSocket | Launch chrome → HTTP GET `/json/version` → WS connect |
| Firefox | WebDriver BiDi | WebSocket | Launch geckodriver → HTTP POST `/session` → WS connect |
| Safari  | WebDriver Classic | HTTP REST | `safaridriver --enable` once → launch safaridriver → HTTP POST `/session` |

All three use JSON messages. Chrome and Firefox use WebSocket (bidirectional).
Safari uses HTTP request/response per operation.

## Dependency Analysis

### Current deps (Chrome/CDP only)
```
fastwebsockets  (WebSocket client - handshake + framing)
hyper           (HTTP/1.1 - used for WS upgrade handshake)
hyper-util      (TokioIo adapter)
http-body-util  (Empty body type)
bytes           (buffer type)
tokio           (async runtime)
serde/serde_json (JSON)
thiserror       (error derive)
tempfile        (Chrome temp profile)
```

### What each backend needs

| Dep | Chrome (CDP) | Firefox (BiDi) | Safari (WebDriver) |
|-----|:---:|:---:|:---:|
| tokio | ✓ | ✓ | ✓ |
| serde + serde_json | ✓ | ✓ | ✓ |
| thiserror | ✓ | ✓ | ✓ |
| tempfile | ✓ | ✓ | ✗ |
| hyper + hyper-util | ✓ (WS upgrade) | ✓ (session + WS upgrade) | ✓ (REST calls) |
| http-body-util + bytes | ✓ | ✓ | ✓ |
| fastwebsockets | ✓ | ✓ | ✗ |

### Key insight: near-total overlap

- `hyper` is needed by all three (WS upgrade for Chrome/Firefox, REST for Safari)
- `fastwebsockets` is needed by Chrome + Firefox (both use WebSocket)
- Safari only adds zero new deps beyond what Chrome already pulls in

**When all features are enabled: zero additional crate dependencies vs Chrome-only.**

Safari's HTTP REST transport is just hyper without the WebSocket upgrade — a
strict subset of what Chrome already uses.

## Cargo Features

```toml
[features]
default = ["chrome", "cli"]
chrome  = ["dep:fastwebsockets"]
firefox = ["dep:fastwebsockets"]
safari  = []                       # no extra deps!
cli     = ["dep:clap", "dep:anyhow"]
```

`fastwebsockets` becomes optional, pulled in by `chrome` or `firefox`.
`hyper` stays unconditional (all backends need HTTP).

## Module Structure

```
src/
  lib.rs              # crate docs, feature-gated re-exports
  error.rs            # shared Error enum
  types.rs            # shared types: JsResult, TargetInfo, BrowserVersion
  launcher.rs         # process launcher (generalized for all browsers)

  transport/
    mod.rs
    ws.rs             # shared WebSocket connect + read/write (chrome+firefox)
    http.rs           # shared HTTP client helpers (all three)

  chrome/
    mod.rs            # pub struct Browser, pub struct Tab
    cdp.rs            # CDP message routing (current cdp.rs logic)

  firefox/
    mod.rs            # pub struct Browser, pub struct Tab
    bidi.rs           # BiDi message routing (similar pattern to cdp.rs)

  safari/
    mod.rs            # pub struct Browser, pub struct Tab
    webdriver.rs      # WebDriver REST client

  main.rs             # CLI with --browser flag
```

## Shared Transport Layer

### `transport::ws` (feature = "chrome" or "firefox")

Extract from current `cdp.rs`:
- `ws_connect(url) -> (FragmentCollectorRead, WebSocketWrite)` — TCP connect,
  HTTP upgrade handshake, split
- Reused identically by CDP and BiDi (both do WS upgrade to localhost)

### `transport::http` (always available)

Simple hyper-based HTTP helpers:
- `http_get(url) -> String` — for Chrome's `/json/version`
- `http_post_json(url, body) -> Value` — for BiDi/WebDriver session creation
- `http_delete(url)` — for WebDriver session teardown
- `http_get_json(url) -> Value` — for WebDriver GET endpoints

These replace the manual TCP HTTP in `browser.rs::get_browser_ws_url()` and
are reused by all three backends.

## Per-Backend Design

### Chrome (CDP) — current code, minor refactor

Move `CdpClient` logic into `chrome/cdp.rs`, extract WS connect into
`transport::ws`. Public API stays the same:

```rust
// src/chrome/mod.rs
pub struct Browser { cdp: Arc<CdpClient>, process: Option<ChromeProcess> }
pub struct Tab { target_id: String, session_id: String, cdp: Arc<CdpClient> }
```

### Firefox (WebDriver BiDi) — new

Session bootstrap:
1. Launch `geckodriver --port PORT`
2. HTTP POST `http://localhost:PORT/session` with `{webSocketUrl: true}`
3. Extract `webSocketUrl` from response capabilities
4. Connect WebSocket (reuse `transport::ws`)

Message format (very similar to CDP):
```json
// Command
{"id": 1, "method": "browsingContext.navigate", "params": {"context": "...", "url": "..."}}
// Response
{"type": "success", "id": 1, "result": {...}}
// Event
{"type": "event", "method": "browsingContext.load", "params": {...}}
```

The BiDi message router is structurally identical to the CDP router:
match on `id` for responses, match on `type: "event"` for events.

```rust
// src/firefox/mod.rs
pub struct Browser { bidi: Arc<BidiClient>, driver_process: Option<Child>, browser_process: Option<Child> }
pub struct Tab { context_id: String, bidi: Arc<BidiClient> }
```

Key BiDi commands:
| Operation | BiDi method |
|-----------|-------------|
| Navigate | `browsingContext.navigate` with `wait: "complete"` |
| Evaluate JS | `script.evaluate` with `target: {context}` |
| New tab | `browsingContext.create` with `type: "tab"` |
| New window | `browsingContext.create` with `type: "window"` |
| Close | `browsingContext.close` |
| List tabs | `browsingContext.getTree` |

### Safari (WebDriver Classic) — new

Session bootstrap:
1. `safaridriver --enable` (one-time, needs user auth)
2. Launch `safaridriver --port PORT`
3. HTTP POST `/session` to create session

No WebSocket — all operations are HTTP REST:

```rust
// src/safari/mod.rs
pub struct Browser { session_id: String, base_url: String, process: Option<Child> }
pub struct Tab { handle: String, session_id: String, base_url: String }
```

Key endpoints:
| Operation | Method + Path |
|-----------|---------------|
| Navigate | POST `/session/{id}/url` body `{"url": "..."}` |
| Evaluate JS | POST `/session/{id}/execute/sync` body `{"script": "return ...", "args": []}` |
| New tab | POST `/session/{id}/window/new` body `{"type": "tab"}` |
| New window | POST `/session/{id}/window/new` body `{"type": "window"}` |
| Close tab | DELETE `/session/{id}/window` |
| List windows | GET `/session/{id}/window/handles` |
| Get title | GET `/session/{id}/title` |
| Get URL | GET `/session/{id}/url` |
| Switch window | POST `/session/{id}/window` body `{"handle": "..."}` |

Note: Safari WebDriver requires switching to a window before operating on it
(unlike CDP/BiDi where you pass target/context ID per command).

**Limitations:**
- No headless mode (Safari always needs a visible window)
- No `set_bounds` equivalent in WebDriver (use `POST /session/{id}/window/rect`)
- No `inject_script` equivalent (Page.addScriptToEvaluateOnNewDocument is CDP-only)
- `evaluate` requires `"return ..."` prefix (unlike CDP's expression evaluation)

## API Surface

Each backend exposes the same struct names and method signatures within its
module. No shared trait needed initially (avoids `async_trait` dep). Users
import from the feature-gated module:

```rust
#[cfg(feature = "chrome")]
use chromerunner::chrome::{Browser, Tab};

#[cfg(feature = "firefox")]
use chromerunner::firefox::{Browser, Tab};

#[cfg(feature = "safari")]
use chromerunner::safari::{Browser, Tab};
```

A trait can be added later if runtime dispatch is needed.

## CLI Changes

```
chromerunner --browser chrome|firefox|safari [--headless] <subcommand>
```

Default: `chrome` (backward compatible).

## Implementation Order

1. **Refactor**: Extract `transport::ws` and `transport::http` from current code
2. **Move**: Current Chrome code into `chrome/` module
3. **Feature-gate**: Make `chrome` a cargo feature, verify existing tests pass
4. **Firefox**: Implement `firefox/` backend using BiDi over shared transport
5. **Safari**: Implement `safari/` backend using WebDriver over shared HTTP
6. **CLI**: Add `--browser` flag
7. **Tests**: Per-backend integration tests, gated by features

## Open Questions

- Should `geckodriver` be auto-downloaded or expected on PATH?
- Safari: `safaridriver --enable` needs user interaction — document as prerequisite?
- Should we add a `BiDi` backend for Chrome too (future-proofing)?
