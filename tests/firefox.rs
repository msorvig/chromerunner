#![cfg(feature = "firefox")]

use chromerunner::firefox::Browser;

async fn launch() -> Browser {
    Browser::launch(true)
        .await
        .expect("failed to launch Firefox — is geckodriver installed?")
}

#[tokio::test]
async fn test_launch_and_version() {
    let browser = launch().await;
    let version = browser.version().await.unwrap();
    assert!(!version.is_null(), "version should not be null");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_new_tab_navigate() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    tab.navigate("data:text/html,<title>Test</title>")
        .await
        .unwrap();
    let title = tab.title().await.unwrap();
    assert_eq!(title, "Test");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_number() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let r = tab.evaluate("1 + 2 + 3").await.unwrap();
    assert_eq!(r.value, serde_json::json!(6));
    assert_eq!(r.result_type, "number");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_string() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let r = tab.evaluate("'hello world'").await.unwrap();
    assert_eq!(r.value, serde_json::json!("hello world"));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_object() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let r = tab.evaluate("({a: 1, b: 'two'})").await.unwrap();
    assert_eq!(r.value["a"], 1);
    assert_eq!(r.value["b"], "two");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_boolean() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let r = tab.evaluate("true && false").await.unwrap();
    assert_eq!(r.value, serde_json::json!(false));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_error() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let r = tab.evaluate("throw new Error('boom')").await;
    assert!(r.is_err());
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_multiple_tabs() {
    let browser = launch().await;
    let tab1 = browser.new_tab("about:blank").await.unwrap();
    let tab2 = browser.new_tab("about:blank").await.unwrap();

    let r1 = tab1.evaluate("1 + 1").await.unwrap();
    let r2 = tab2.evaluate("2 + 2").await.unwrap();
    assert_eq!(r1.value, serde_json::json!(2));
    assert_eq!(r2.value, serde_json::json!(4));

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_new_window() {
    let browser = launch().await;
    let win = browser
        .new_window("data:text/html,<title>Win</title>")
        .await
        .unwrap();
    let title = win.title().await.unwrap();
    assert_eq!(title, "Win");
    win.close().await.unwrap();
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_close_tab() {
    let browser = launch().await;
    let _tab1 = browser.new_tab("about:blank").await.unwrap();
    let tab2 = browser.new_tab("about:blank").await.unwrap();

    let before = browser.list_targets().await.unwrap().len();
    tab2.close().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let after = browser.list_targets().await.unwrap().len();
    assert!(after < before);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_list_targets() {
    let browser = launch().await;
    browser.new_tab("about:blank").await.unwrap();
    browser.new_tab("about:blank").await.unwrap();

    let targets = browser.list_targets().await.unwrap();
    assert!(
        targets.len() >= 2,
        "expected >=2 targets, got {}",
        targets.len()
    );

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Isolated browser contexts (BiDi user contexts)
// ---------------------------------------------------------------------------

/// Start a minimal localhost HTTP server and return its base URL.
/// localStorage and cookies need a real origin, so `about:blank` won't do.
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
                    let body = "<html><body></body></html>";
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

#[tokio::test]
async fn test_context_isolates_localstorage() {
    let base_url = start_local_server().await;
    let browser = launch().await;

    let ctx_a = browser.new_context().await.unwrap();
    let ctx_b = browser.new_context().await.unwrap();
    assert_ne!(ctx_a.id(), ctx_b.id());

    let tab_a = ctx_a.new_tab(&base_url).await.unwrap();
    let tab_b = ctx_b.new_tab(&base_url).await.unwrap();

    tab_a
        .evaluate("localStorage.setItem('k', 'from_a')")
        .await
        .unwrap();

    let seen_by_b = tab_b.evaluate("localStorage.getItem('k')").await.unwrap();
    assert_eq!(seen_by_b.value, serde_json::json!(null));

    let tab_a2 = ctx_a.new_tab(&base_url).await.unwrap();
    let seen_by_a2 = tab_a2.evaluate("localStorage.getItem('k')").await.unwrap();
    assert_eq!(seen_by_a2.value, serde_json::json!("from_a"));

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_context_close_removes_tabs() {
    let base_url = start_local_server().await;
    let browser = launch().await;

    let ctx = browser.new_context().await.unwrap();
    let tab = ctx.new_tab(&base_url).await.unwrap();
    let context_id = tab.target_id().to_string();

    let targets = browser.list_targets().await.unwrap();
    assert!(targets.iter().any(|t| t.target_id == context_id));

    ctx.close().await.unwrap();

    let targets = browser.list_targets().await.unwrap();
    assert!(
        !targets.iter().any(|t| t.target_id == context_id),
        "tab survived its user context being removed"
    );

    browser.close().await.unwrap();
}
