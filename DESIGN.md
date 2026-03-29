# Internal Design

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                   Your application                   │
│          fn scrape(b: &impl BrowserApi) { }          │
├──────────────────────────────────────────────────────┤
│          BrowserApi + TabApi  (shared traits)         │
├────────────────┬─────────────────┬───────────────────┤
│ chrome::Browser│ firefox::Browser│ safari::Browser   │
│ chrome::Tab    │ firefox::Tab    │ safari::Tab       │
│ (CDP)          │ (BiDi)          │ (WebDriver REST)  │
├────────────────┴─────────────────┴───────────────────┤
│        Shared types (JsResult, TargetInfo, Error)     │
├──────────────────────┬───────────────────────────────┤
│  transport::ws       │  transport::http              │
│  (fastwebsockets)    │  (hyper)                      │
│  Chrome + Firefox    │  all three backends           │
└──────────────────────┴───────────────────────────────┘
```

## Module layout

```
src/
  lib.rs              # crate docs, feature-gated re-exports
  error.rs            # shared Error enum
  types.rs            # JsResult, TargetInfo, BrowserApi, TabApi traits
  launcher.rs         # process launcher for all three browsers

  transport/
    mod.rs
    ws.rs             # WebSocket connect + split (chrome + firefox)
    http.rs           # HTTP client helpers (all three backends)

  chrome/
    mod.rs            # Browser, Tab — high-level CDP API
    cdp.rs            # CdpClient — WebSocket message router

  firefox/
    mod.rs            # Browser, Tab — high-level BiDi API
    bidi.rs           # BidiClient — WebSocket message router

  safari/
    mod.rs            # Browser, Tab — WebDriver REST API

  main.rs             # CLI binary (behind "cli" feature)
```

## Protocol details

### Session bootstrap

| Browser | Steps |
|---------|-------|
| Chrome  | Launch chrome `--remote-debugging-port=PORT` → poll `GET /json/version` → WebSocket connect |
| Firefox | Launch geckodriver `--port PORT --websocket-port WSPORT` → poll `GET /status` → `POST /session` with `webSocketUrl: true` → WebSocket connect to returned URL |
| Safari  | Launch `safaridriver --port PORT` → poll `GET /status` → `POST /session` |

### Message routing (Chrome CDP)

CDP uses JSON-RPC over WebSocket:

```
Command:  {"id": 1, "method": "Runtime.evaluate", "params": {...}, "sessionId": "..."}
Response: {"id": 1, "result": {...}}
Error:    {"id": 1, "error": {"code": -32000, "message": "..."}}
Event:    {"method": "Page.loadEventFired", "params": {...}}
```

`CdpClient` runs a background tokio task that reads frames and routes:
- Messages with `id` → matched to pending `oneshot::Sender` by id
- Messages with `method` (no `id`) → broadcast to event subscribers

The write half is behind `Mutex<WsWriter>` for concurrent sends.

### Message routing (Firefox BiDi)

BiDi is structurally similar but uses a `type` discriminant:

```
Command:  {"id": 1, "method": "browsingContext.navigate", "params": {...}}
Response: {"type": "success", "id": 1, "result": {...}}
Error:    {"type": "error", "id": 1, "error": "...", "message": "..."}
Event:    {"type": "event", "method": "browsingContext.load", "params": {...}}
```

`BidiClient` uses the same architecture as `CdpClient` (background read task,
oneshot channels for responses, broadcast for events).

### Safari WebDriver

No WebSocket. Every operation is a synchronous HTTP REST call:

```
Navigate:  POST /session/{id}/url         {"url": "..."}
Evaluate:  POST /session/{id}/execute/sync {"script": "return ...", "args": []}
New tab:   POST /session/{id}/window/new  {"type": "tab"}
Close:     DELETE /session/{id}/window
```

Safari requires switching to a window before operating on it. `Tab` methods
call `switch_to()` automatically before each operation.

### Safari evaluate workarounds

WebDriver `execute/sync` requires `return`. The library wraps expressions:
- `"2 + 2"` → `"return (2 + 2)"`
- Multi-statement (contains `;`): `"x(); y()"` → `"return (function(){ x(); y() })()"`

### BiDi value conversion

BiDi returns values in its own format:
```json
{"type": "number", "value": 4}
{"type": "object", "value": [["key", {"type": "string", "value": "val"}]]}
```

`bidi_to_json()` converts to `serde_json::Value`, preserving integers
(BiDi uses f64 internally but we emit `json!(n as i64)` when there's no
fractional part).

## Dependency strategy

| Dep | Chrome | Firefox | Safari | Why |
|-----|:------:|:-------:|:------:|-----|
| tokio | ✓ | ✓ | ✓ | async runtime |
| serde + serde_json | ✓ | ✓ | ✓ | all protocols are JSON |
| hyper + hyper-util | ✓ | ✓ | ✓ | HTTP for WS upgrade + REST |
| http-body-util + bytes | ✓ | ✓ | ✓ | hyper body types |
| fastwebsockets | ✓ | ✓ | - | WebSocket framing |
| tracing | ✓ | ✓ | ✓ | instrumentation (zero-cost when no subscriber) |
| tempfile | ✓ | - | - | Chrome temp profile dir |
| thiserror | ✓ | ✓ | ✓ | error derive |

`fastwebsockets` is the only conditional dep (behind `chrome`/`firefox` features).
Enabling `safari` adds zero additional crates.

## Trait design

`BrowserApi` and `TabApi` use Rust's native `async fn` in traits (stable
since 1.75). No `async_trait` crate needed. The associated type
`BrowserApi::Tab: TabApi` connects the two.

This supports static dispatch (`impl BrowserApi`, `<B: BrowserApi>`) but not
dynamic dispatch (`dyn BrowserApi`) — the return types differ per impl.
Runtime browser selection uses a `match`:

```rust
match name {
    "chrome"  => run::<chrome::Browser>().await,
    "firefox" => run::<firefox::Browser>().await,
    "safari"  => run::<safari::Browser>().await,
}
```

## Process lifecycle

All browser/driver processes are wrapped in `ChildGuard` which kills on drop.
Chrome also gets a `TempDir` for its user-data directory (cleaned up on drop).
geckodriver manages its own temp Firefox profile.
