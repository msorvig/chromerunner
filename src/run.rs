//! `chromerunner run`: open a page in Chrome, stream its console, wait for a
//! condition, and optionally take a screenshot. Meant as a build/run loop for
//! web apps (e.g. Qt for WebAssembly): one command gives the page's output,
//! a pass/fail exit code, and a picture of what it rendered.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use chromerunner::chrome::{Browser, Tab};

#[derive(clap::Args)]
pub struct RunArgs {
    /// URL to open. With --serve, a path relative to the served directory.
    pub url: String,

    /// Serve this directory over http on a free localhost port (no caching,
    /// COOP/COEP set so SharedArrayBuffer works).
    #[arg(long)]
    pub serve: Option<PathBuf>,

    /// Extra Chrome command-line flag (repeatable), e.g. --flag=--enable-unsafe-webgpu
    #[arg(long = "flag", allow_hyphen_values = true)]
    pub flags: Vec<String>,

    /// JavaScript expression polled until truthy
    #[arg(long)]
    pub wait_for: Option<String>,

    /// Finish when a console message contains this text
    #[arg(long)]
    pub until_console: Option<String>,

    /// Give up after this many seconds. Without a wait condition, this is how
    /// long the page runs.
    #[arg(long, default_value_t = 20.0)]
    pub timeout: f64,

    /// Keep running this many milliseconds after the condition is met
    /// (lets a frame or two render before the screenshot)
    #[arg(long, default_value_t = 500)]
    pub settle: u64,

    /// Write a PNG screenshot of the viewport here at the end
    #[arg(long)]
    pub screenshot: Option<PathBuf>,

    /// Viewport size, WIDTHxHEIGHT
    #[arg(long)]
    pub size: Option<String>,

    /// Device scale factor (window.devicePixelRatio) to emulate, default 1.
    /// The page can change size and scale while running by logging
    /// "AGENTEMU:<W>x<H>@<DPR>" to the console, and emulate CSS media
    /// features by logging "AGENTMEDIA:<name>=<value>,..." (e.g.
    /// "AGENTMEDIA:color-gamut=p3,dynamic-range=high"; "AGENTMEDIA:" resets),
    /// and click by logging "AGENTCLICK:<x>,<y>" (CSS pixels, trusted events).
    #[arg(long)]
    pub dpr: Option<f64>,

    /// Evaluate this expression at the end and print the result
    #[arg(long)]
    pub eval: Option<String>,

    /// Stop and fail on an uncaught exception (e.g. a wasm abort)
    #[arg(long)]
    pub fail_on_exception: bool,

    /// Keep the browser open until Ctrl-C after finishing
    #[arg(long)]
    pub keep_open: bool,

    /// Take a screenshot into this directory whenever a console message
    /// contains "AGENTSHOT:<name>" (written as <name>.png)
    #[arg(long)]
    pub shot_on_console: Option<PathBuf>,
}

pub async fn run(headless: bool, args: RunArgs) -> anyhow::Result<()> {
    let base_url = match &args.serve {
        Some(dir) => {
            let dir = dir
                .canonicalize()
                .with_context(|| format!("serve dir {}", dir.display()))?;
            let port = serve(dir).await?;
            Some(format!("http://127.0.0.1:{port}"))
        }
        None => None,
    };
    let url = match &base_url {
        Some(base) => format!("{base}/{}", args.url.trim_start_matches('/')),
        None => args.url.clone(),
    };

    let flags: Vec<&str> = args.flags.iter().map(|s| s.as_str()).collect();
    let browser = Browser::launch_with_args(headless, &flags).await?;
    let result = drive(&browser, &url, &args).await;
    if args.keep_open {
        eprintln!("[chromerunner] finished; browser kept open, Ctrl-C to quit");
        let _ = tokio::signal::ctrl_c().await;
    }
    browser.close().await?;
    result
}

async fn drive(browser: &Browser, url: &str, args: &RunArgs) -> anyhow::Result<()> {
    let tab = browser.new_tab("about:blank").await?;
    let cdp = browser.cdp().clone();
    let sid = tab.session_id().to_string();

    if args.size.is_some() || args.dpr.is_some() {
        let (w, h) = match &args.size {
            Some(size) => size
                .split_once('x')
                .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
                .context("--size must be WIDTHxHEIGHT")?,
            None => (0, 0), // 0 keeps the window's size
        };
        cdp.send(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": w, "height": h, "deviceScaleFactor": args.dpr.unwrap_or(1.0), "mobile": false }),
            Some(&sid),
        )
        .await?;
    }

    // Console streaming: subscribe before enabling so nothing is missed.
    let matched = Arc::new(AtomicBool::new(false));
    let errored = Arc::new(AtomicBool::new(false));
    let crashed = Arc::new(AtomicBool::new(false));
    let mut events = cdp.subscribe();
    let (shot_tx, mut shot_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (emu_tx, mut emu_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        let shot_tx = shot_tx.clone();
        let sid = sid.clone();
        let needle = args.until_console.clone();
        let (matched, errored, crashed) = (matched.clone(), errored.clone(), crashed.clone());
        tokio::spawn(async move {
            while let Ok(ev) = events.recv().await {
                if ev.session_id.as_deref() != Some(sid.as_str()) {
                    continue;
                }
                let line = match ev.method.as_str() {
                    "Runtime.consoleAPICalled" => {
                        let kind = ev.params["type"].as_str().unwrap_or("log").to_string();
                        let text = ev.params["args"]
                            .as_array()
                            .map(|a| a.iter().map(remote_to_string).collect::<Vec<_>>().join(" "))
                            .unwrap_or_default();
                        Some((kind, text))
                    }
                    "Runtime.exceptionThrown" => {
                        errored.store(true, Ordering::SeqCst);
                        let d = &ev.params["exceptionDetails"];
                        let text = d["exception"]["description"]
                            .as_str()
                            .or(d["text"].as_str())
                            .unwrap_or("exception")
                            .to_string();
                        Some(("exception".into(), text))
                    }
                    "Log.entryAdded" => {
                        let e = &ev.params["entry"];
                        let level = e["level"].as_str().unwrap_or("info");
                        let text = e["text"].as_str().unwrap_or("").to_string();
                        let src = e["url"].as_str().unwrap_or("");
                        Some((format!("browser-{level}"), if src.is_empty() { text } else { format!("{text} ({src})") }))
                    }
                    "Inspector.targetCrashed" => {
                        crashed.store(true, Ordering::SeqCst);
                        Some(("crash".into(), "renderer crashed".into()))
                    }
                    _ => None,
                };
                if let Some((kind, text)) = line {
                    for l in text.lines() {
                        println!("[{kind}] {l}");
                    }
                    if let Some(i) = text.find("AGENTRESIZE:") {
                        let v = text[i + 12..].split_whitespace().next().unwrap_or("").to_string();
                        let _ = shot_tx.send(format!("@resize {v}"));
                    }
                    if let Some(i) = text.find("AGENTSHOT:") {
                        let name = text[i + 10..].split_whitespace().next().unwrap_or("shot").to_string();
                        let _ = shot_tx.send(name);
                    }
                    if let Some(i) = text.find("AGENTEMU:") {
                        let spec = text[i + 9..].split_whitespace().next().unwrap_or("").to_string();
                        let _ = emu_tx.send(spec);
                    }
                    if let Some(i) = text.find("AGENTCLICK:") {
                        let spec = text[i + 11..].split_whitespace().next().unwrap_or("").to_string();
                        let _ = emu_tx.send(format!("@click {spec}"));
                    }
                    if let Some(i) = text.find("AGENTMEDIA:") {
                        let spec = text[i + 11..].split_whitespace().next().unwrap_or("").to_string();
                        let _ = emu_tx.send(format!("@media {spec}"));
                    }
                    if let Some(n) = &needle {
                        if text.contains(n.as_str()) {
                            matched.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        });
    }
    for m in ["Runtime.enable", "Log.enable", "Inspector.enable", "Page.enable"] {
        cdp.send(m, json!({}), Some(&sid)).await?;
    }

    eprintln!("[chromerunner] open {url}");
    // Page.navigate returns once the navigation commits; don't wait for load,
    // an app page may never reach readyState=complete in a useful way.
    let nav = cdp
        .send("Page.navigate", json!({ "url": url }), Some(&sid))
        .await?;
    if let Some(err) = nav["errorText"].as_str() {
        bail!("navigation failed: {err}");
    }

    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(args.timeout);
    let has_condition = args.wait_for.is_some() || args.until_console.is_some();
    let mut ok = !has_condition;
    loop {
        while let Ok(spec) = emu_rx.try_recv() {
            if let Some(at) = spec.strip_prefix("@click ") {
                // <x>,<y> in CSS pixels: a trusted left click (press + release)
                match at.split_once(',').and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))) {
                    Some((x, y)) => {
                        for kind in ["mousePressed", "mouseReleased"] {
                            cdp.send("Input.dispatchMouseEvent",
                                json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1 }),
                                Some(&sid)).await?;
                        }
                        eprintln!("[chromerunner] click {x},{y}");
                    }
                    None => eprintln!("[chromerunner] bad AGENTCLICK spec '{at}', want <x>,<y>"),
                }
                continue;
            }
            if let Some(media) = spec.strip_prefix("@media ") {
                // <name>=<value>,... ; empty resets all emulated features
                let features: Vec<_> = media
                    .split(',')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(n, v)| json!({ "name": n, "value": v }))
                    .collect();
                cdp.send("Emulation.setEmulatedMedia", json!({ "features": features }), Some(&sid))
                    .await?;
                eprintln!("[chromerunner] emulating media {}", if media.is_empty() { "(reset)" } else { media });
                continue;
            }
            // <W>x<H>@<DPR>
            let parsed = spec.split_once('@').and_then(|(size, dpr)| {
                let (w, h) = size.split_once('x')?;
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?, dpr.parse::<f64>().ok()?))
            });
            match parsed {
                Some((w, h, dpr)) => {
                    cdp.send(
                        "Emulation.setDeviceMetricsOverride",
                        json!({ "width": w, "height": h, "deviceScaleFactor": dpr, "mobile": false }),
                        Some(&sid),
                    )
                    .await?;
                    eprintln!("[chromerunner] emulating {w}x{h} dpr {dpr}");
                }
                None => eprintln!("[chromerunner] bad AGENTEMU spec '{spec}', want <W>x<H>@<DPR>"),
            }
        }
        while let Ok(name) = shot_rx.try_recv() {
            if let Some(sz0) = name.strip_prefix("@resize ") {
                let (sz, dpr) = match sz0.split_once('@') { Some((a, d)) => (a, d.parse::<f64>().unwrap_or(1.0)), None => (sz0, 1.0) };
                if let Some((w, h)) = sz.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))) {
                    cdp.send("Emulation.setDeviceMetricsOverride",
                        json!({ "width": w, "height": h, "deviceScaleFactor": dpr, "mobile": false }), Some(&sid)).await?;
                    eprintln!("[chromerunner] viewport {w}x{h}@{dpr}");
                }
                continue;
            }
            if let Some(dir) = &args.shot_on_console {
                let path = dir.join(format!("{name}.png"));
                match tab.screenshot().await {
                    Ok(png) => {
                        std::fs::create_dir_all(dir).ok();
                        std::fs::write(&path, &png).with_context(|| format!("write {}", path.display()))?;
                        eprintln!("[chromerunner] screenshot {}", path.display());
                    }
                    Err(e) => eprintln!("[chromerunner] screenshot {name} failed: {e}"),
                }
            }
        }
        if crashed.load(Ordering::SeqCst) {
            ok = false;
            break;
        }
        if args.fail_on_exception && errored.load(Ordering::SeqCst) {
            ok = false;
            break;
        }
        if matched.load(Ordering::SeqCst) {
            ok = true;
            break;
        }
        if let Some(expr) = &args.wait_for {
            if let Ok(r) = tab.evaluate(&format!("!!({expr})")).await {
                if r.value == Value::Bool(true) {
                    ok = true;
                    break;
                }
            }
        }
        if Instant::now() >= deadline {
            if has_condition {
                eprintln!("[chromerunner] timed out after {:.1}s waiting for condition", args.timeout);
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if ok && has_condition {
        eprintln!("[chromerunner] condition met after {:.1}s", start.elapsed().as_secs_f64());
    }
    if !crashed.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(args.settle)).await;
        finish(&tab, args).await?;
    }

    if crashed.load(Ordering::SeqCst) {
        bail!("renderer crashed");
    }
    if args.fail_on_exception && errored.load(Ordering::SeqCst) {
        bail!("uncaught exception");
    }
    if !ok {
        bail!("condition not met");
    }
    Ok(())
}

async fn finish(tab: &Tab, args: &RunArgs) -> anyhow::Result<()> {
    if let Some(expr) = &args.eval {
        match tab.evaluate(expr).await {
            Ok(r) => println!("[eval] {}", r.value),
            Err(e) => println!("[eval] error: {e}"),
        }
    }
    if let Some(path) = &args.screenshot {
        let png = tab.screenshot().await?;
        std::fs::write(path, &png).with_context(|| format!("write {}", path.display()))?;
        eprintln!("[chromerunner] screenshot {} ({} bytes)", path.display(), png.len());
    }
    Ok(())
}

/// Render a CDP RemoteObject roughly the way the DevTools console would.
fn remote_to_string(o: &Value) -> String {
    if let Some(v) = o.get("value") {
        return match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    if let Some(u) = o.get("unserializableValue").and_then(|v| v.as_str()) {
        return u.to_string();
    }
    o.get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| o["type"].as_str().unwrap_or("?"))
        .to_string()
}

// ---------------------------------------------------------------------------
// Static file server
// ---------------------------------------------------------------------------

async fn serve(root: PathBuf) -> anyhow::Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    eprintln!("[chromerunner] serving {} on http://127.0.0.1:{port}", root.display());
    let root = Arc::new(root);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let root = root.clone();
            tokio::spawn(async move {
                let _ = handle(stream, &root).await;
            });
        }
    });
    Ok(port)
}

async fn handle(mut stream: tokio::net::TcpStream, root: &Path) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).await?;
        if n == 0 || buf.len() > 64 * 1024 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let req = String::from_utf8_lossy(&buf);
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let path = percent_decode(path);

    let file = if path.split('/').any(|c| c == "..") {
        None
    } else {
        let mut p = root.join(path.trim_start_matches('/'));
        if p.is_dir() {
            p = p.join("index.html");
        }
        std::fs::read(&p).ok().map(|b| (b, content_type(&p)))
    };
    let (status, body, ctype) = match file {
        Some((b, t)) => ("200 OK", b, t),
        // Browsers ask for it unprompted; a 404 would show up as console noise.
        None if path == "/favicon.ico" => ("204 No Content", Vec::new(), "image/x-icon"),
        None => ("404 Not Found", b"not found".to_vec(), "text/plain"),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Cross-Origin-Opener-Policy: same-origin\r\n\
         Cross-Origin-Embedder-Policy: require-corp\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    if method != "HEAD" {
        stream.write_all(&body).await?;
    }
    stream.shutdown().await
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
