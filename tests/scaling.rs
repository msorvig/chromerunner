//! Concurrency scaling probe: isolated contexts vs. separate browsers.
//!
//! Not part of the normal test run — these take minutes and are sensitive to
//! machine load. Run explicitly:
//!
//! ```text
//! cargo test --test scaling -- --ignored --nocapture --test-threads=1
//! ```
#![cfg(feature = "chrome")]

use chromerunner::Browser;
use std::sync::Arc;
use std::time::Instant;

/// Start a minimal localhost HTTP server and return its base URL.
async fn start_local_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let body = "<html><head><title>probe</title></head><body></body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                         Content-Type: text/html\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes()).await;
                });
            }
        }
    });
    format!("http://127.0.0.1:{}", port)
}

/// Resident memory of every descendant of this test process, in MiB, plus the
/// process count. Walking the tree from our own pid is what keeps unrelated
/// Chrome instances on the machine — the user's own browser, most of all — out
/// of the number. Linux-only; returns zeroes elsewhere.
fn descendant_rss_mib() -> (u64, usize) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return (0, 0);
    };

    // pid -> (ppid, resident bytes)
    let mut procs: std::collections::HashMap<u32, (u32, u64)> = std::collections::HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        // `stat` field 4 is ppid, but comm (field 2) can contain spaces and
        // parentheses, so parse from the last ')' rather than splitting.
        let Ok(stat) = std::fs::read_to_string(path.join("stat")) else {
            continue;
        };
        let Some(after_comm) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
            continue;
        };
        let Some(ppid) = after_comm
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let rss = std::fs::read_to_string(path.join("statm"))
            .ok()
            .and_then(|statm| {
                statm
                    .split_whitespace()
                    .nth(1) // resident pages
                    .and_then(|p| p.parse::<u64>().ok())
            })
            .unwrap_or(0)
            * 4096;
        procs.insert(pid, (ppid, rss));
    }

    let me = std::process::id();
    let (mut total, mut count) = (0, 0);
    for (&pid, &(_, rss)) in &procs {
        if pid == me {
            continue;
        }
        // Walk up to see whether we are an ancestor. Bounded to guard against
        // a cycle from a pid reused mid-scan.
        let mut cursor = pid;
        for _ in 0..64 {
            let Some(&(ppid, _)) = procs.get(&cursor) else {
                break;
            };
            if ppid == me {
                total += rss;
                count += 1;
                break;
            }
            if ppid <= 1 {
                break;
            }
            cursor = ppid;
        }
    }
    (total / (1024 * 1024), count)
}

/// N isolated contexts inside one browser, driven concurrently.
async fn run_contexts(label: &str, sizes: &[usize]) {
    let url = start_local_server().await;

    println!("\n--- N isolated contexts in ONE browser [{label}] ---");
    for &n in sizes {
        let launch_start = Instant::now();
        let browser = Arc::new(Browser::launch(true).await.unwrap());
        let launch_ms = launch_start.elapsed().as_millis();

        let start = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..n {
            let browser = browser.clone();
            let url = url.clone();
            set.spawn(async move {
                let context = browser.new_context().await.map_err(|e| e.to_string())?;
                let tab = context
                    .new_tab(&format!("{}/?c={}", url, i))
                    .await
                    .map_err(|e| e.to_string())?;
                // Each context must see only its own storage.
                tab.evaluate(&format!("localStorage.setItem('id', '{}')", i))
                    .await
                    .map_err(|e| e.to_string())?;
                let seen = tab
                    .evaluate("localStorage.getItem('id')")
                    .await
                    .map_err(|e| e.to_string())?;
                let value = seen.value == serde_json::json!(i.to_string());
                context.close().await.map_err(|e| e.to_string())?;
                Ok::<_, String>(value)
            });
        }

        let (mut ok, mut failed, mut leaked) = (0, 0, 0);
        let mut first_error = String::new();
        let mut peak_rss = 0;
        let mut peak_procs = 0;
        while let Some(result) = set.join_next().await {
            match result.unwrap() {
                Ok(true) => ok += 1,
                Ok(false) => leaked += 1,
                Err(e) => {
                    if first_error.is_empty() {
                        first_error = e;
                    }
                    failed += 1;
                }
            }
            let (rss, procs) = descendant_rss_mib();
            peak_rss = peak_rss.max(rss);
            peak_procs = peak_procs.max(procs);
        }
        let work_ms = start.elapsed().as_millis();
        println!(
            "n={n:<4} launch {launch_ms:>5}ms  work {work_ms:>6}ms ({:>5.1}ms/ctx)  \
             ok={ok} failed={failed} storage_leaks={leaked}  \
             peak {peak_rss}MiB/{peak_procs}procs {first_error}",
            work_ms as f64 / n as f64,
        );

        Arc::try_unwrap(browser)
            .unwrap_or_else(|_| panic!("browser still shared"))
            .close()
            .await
            .unwrap();
    }
}

/// N separate browser processes, launched concurrently.
async fn run_browsers(label: &str, sizes: &[usize]) {
    let url = start_local_server().await;

    println!("\n--- N separate BROWSERS [{label}] ---");
    for &n in sizes {
        let start = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..n {
            let url = url.clone();
            set.spawn(async move {
                let browser = Browser::launch(true)
                    .await
                    .map_err(|e| format!("launch: {e}"))?;
                let tab = browser
                    .new_tab(&format!("{}/?b={}", url, i))
                    .await
                    .map_err(|e| format!("new_tab: {e}"))?;
                tab.evaluate("1 + 1")
                    .await
                    .map_err(|e| format!("evaluate: {e}"))?;
                browser.close().await.map_err(|e| format!("close: {e}"))?;
                Ok::<_, String>(())
            });
        }

        let (mut ok, mut failed) = (0, 0);
        let mut first_error = String::new();
        let mut peak_rss = 0;
        let mut peak_procs = 0;
        while let Some(result) = set.join_next().await {
            match result.unwrap() {
                Ok(()) => ok += 1,
                Err(e) => {
                    if first_error.is_empty() {
                        first_error = e;
                    }
                    failed += 1;
                }
            }
            let (rss, procs) = descendant_rss_mib();
            peak_rss = peak_rss.max(rss);
            peak_procs = peak_procs.max(procs);
        }
        let elapsed = start.elapsed().as_millis();
        println!(
            "n={n:<4} {elapsed:>6}ms ({:>6.1}ms/browser)  ok={ok} failed={failed}  \
             peak {peak_rss}MiB/{peak_procs}procs {first_error}",
            elapsed as f64 / n as f64,
        );
    }
}

/// N plain tabs in one browser's default context — no isolation, as a
/// baseline for what `new_context` itself costs.
async fn run_tabs(label: &str, sizes: &[usize]) {
    let url = start_local_server().await;

    println!("\n--- N plain tabs in ONE browser, no isolation [{label}] ---");
    for &n in sizes {
        let browser = Arc::new(Browser::launch(true).await.unwrap());

        let start = Instant::now();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..n {
            let browser = browser.clone();
            let url = url.clone();
            set.spawn(async move {
                let tab = browser
                    .new_tab(&format!("{}/?t={}", url, i))
                    .await
                    .map_err(|e| e.to_string())?;
                tab.evaluate(&format!("localStorage.setItem('id', '{}')", i))
                    .await
                    .map_err(|e| e.to_string())?;
                tab.evaluate("localStorage.getItem('id')")
                    .await
                    .map_err(|e| e.to_string())?;
                tab.close().await.map_err(|e| e.to_string())?;
                Ok::<_, String>(())
            });
        }

        let (mut ok, mut failed) = (0, 0);
        let mut first_error = String::new();
        while let Some(result) = set.join_next().await {
            match result.unwrap() {
                Ok(()) => ok += 1,
                Err(e) => {
                    if first_error.is_empty() {
                        first_error = e;
                    }
                    failed += 1;
                }
            }
        }
        let elapsed = start.elapsed().as_millis();
        println!(
            "n={n:<4} {elapsed:>6}ms ({:>5.1}ms/tab)  ok={ok} failed={failed}  {first_error}",
            elapsed as f64 / n as f64,
        );

        Arc::try_unwrap(browser)
            .unwrap_or_else(|_| panic!("browser still shared"))
            .close()
            .await
            .unwrap();
    }
}

const SIZES: &[usize] = &[1, 8, 16, 32, 64, 128];

#[tokio::test]
#[ignore = "scaling probe; run with --ignored"]
async fn scaling_tabs_no_isolation() {
    run_tabs("current_thread runtime", SIZES).await;
}

#[tokio::test]
#[ignore = "scaling probe; run with --ignored"]
async fn scaling_contexts_single_threaded() {
    run_contexts("current_thread runtime", SIZES).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "scaling probe; run with --ignored"]
async fn scaling_contexts_multi_threaded() {
    run_contexts("multi_thread runtime, 8 workers", SIZES).await;
}

#[tokio::test]
#[ignore = "scaling probe; run with --ignored"]
async fn scaling_browsers_single_threaded() {
    run_browsers("current_thread runtime", SIZES).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "scaling probe; run with --ignored"]
async fn scaling_browsers_multi_threaded() {
    run_browsers("multi_thread runtime, 8 workers", SIZES).await;
}
