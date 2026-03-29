use chromerunner::Browser;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Launch a headless browser for testing.
async fn launch() -> Browser {
    Browser::launch(true)
        .await
        .expect("failed to launch Chrome — is it installed?")
}

/// Start a minimal localhost HTTP server and return its base URL.
/// Required for tests that need a real origin (localStorage, BroadcastChannel).
async fn start_local_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
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

// ---------------------------------------------------------------------------
// Launch & version
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_launch_and_version() {
    let browser = launch().await;
    let version = browser.version().await.unwrap();
    let product = version["product"].as_str().unwrap();
    assert!(
        product.contains("Chrome") || product.contains("Headless"),
        "unexpected product: {}",
        product
    );
    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_new_tab_navigate() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    tab.navigate("data:text/html,<title>Test Page</title><p>content</p>")
        .await
        .unwrap();
    let title = tab.title().await.unwrap();
    assert_eq!(title, "Test Page");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_navigate_updates_url() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    let url1 = tab.url().await.unwrap();
    assert_eq!(url1, "about:blank");

    tab.navigate("data:text/html,<title>New</title>")
        .await
        .unwrap();
    let url2 = tab.url().await.unwrap();
    assert!(url2.starts_with("data:"), "expected data URL, got {}", url2);

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_multiple_navigations() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    for i in 0..3 {
        let html = format!("data:text/html,<title>Page {}</title>", i);
        tab.navigate(&html).await.unwrap();
        let title = tab.title().await.unwrap();
        assert_eq!(title, format!("Page {}", i));
    }

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// JavaScript evaluation — types
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_evaluate_number() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("1 + 2 + 3").await.unwrap();
    assert_eq!(result.value, serde_json::json!(6));
    assert_eq!(result.result_type, "number");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_string() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("'hello' + ' ' + 'world'").await.unwrap();
    assert_eq!(result.value, serde_json::json!("hello world"));
    assert_eq!(result.result_type, "string");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_object() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("({a: 1, b: 'two'})").await.unwrap();
    assert_eq!(result.value["a"], 1);
    assert_eq!(result.value["b"], "two");
    assert_eq!(result.result_type, "object");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_array() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("[1, 'two', true]").await.unwrap();
    let arr = result.value.as_array().expect("expected array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], 1);
    assert_eq!(arr[1], "two");
    assert_eq!(arr[2], true);
    assert_eq!(result.result_type, "object"); // arrays are objects in JS
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_boolean() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("true && false").await.unwrap();
    assert_eq!(result.value, serde_json::json!(false));
    assert_eq!(result.result_type, "boolean");
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_null_undefined() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    let result = tab.evaluate("null").await.unwrap();
    assert_eq!(result.value, serde_json::Value::Null);
    assert_eq!(result.result_type, "object"); // typeof null === "object"

    let result = tab.evaluate("undefined").await.unwrap();
    assert_eq!(result.value, serde_json::Value::Null); // undefined → JSON null
    assert_eq!(result.result_type, "undefined");

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_promise() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab
        .evaluate("new Promise(resolve => setTimeout(() => resolve(42), 10))")
        .await
        .unwrap();
    assert_eq!(result.value, serde_json::json!(42));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_evaluate_error() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();
    let result = tab.evaluate("throw new Error('boom')").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("boom"), "error should mention 'boom': {}", err);
    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// DOM interaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_evaluate_dom() {
    let browser = launch().await;
    let tab = browser
        .new_tab("data:text/html,<div id='x'>hello</div>")
        .await
        .unwrap();
    let result = tab
        .evaluate("document.getElementById('x').textContent")
        .await
        .unwrap();
    assert_eq!(result.value, serde_json::json!("hello"));
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_dom_mutation() {
    let browser = launch().await;
    let tab = browser
        .new_tab("data:text/html,<p id='msg'>before</p>")
        .await
        .unwrap();

    tab.evaluate("document.getElementById('msg').textContent = 'after'")
        .await
        .unwrap();

    let result = tab
        .evaluate("document.getElementById('msg').textContent")
        .await
        .unwrap();
    assert_eq!(result.value, serde_json::json!("after"));

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Tabs & windows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_multiple_tabs_same_context() {
    let browser = launch().await;

    let tab1 = browser.new_tab("about:blank").await.unwrap();
    let tab2 = browser.new_tab("about:blank").await.unwrap();

    // Both tabs can evaluate JS independently
    let r1 = tab1.evaluate("1 + 1").await.unwrap();
    let r2 = tab2.evaluate("2 + 2").await.unwrap();
    assert_eq!(r1.value, serde_json::json!(2));
    assert_eq!(r2.value, serde_json::json!(4));

    // Verify both appear in the target list
    let targets = browser.list_targets().await.unwrap();
    let page_ids: Vec<_> = targets
        .iter()
        .filter(|t| t.target_type == "page")
        .map(|t| t.target_id.clone())
        .collect();
    assert!(page_ids.contains(&tab1.target_id().to_string()));
    assert!(page_ids.contains(&tab2.target_id().to_string()));

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

    let tab1 = browser.new_tab("about:blank").await.unwrap();
    let tab2 = browser.new_tab("about:blank").await.unwrap();

    let initial_pages = browser
        .list_targets()
        .await
        .unwrap()
        .iter()
        .filter(|t| t.target_type == "page")
        .count();

    tab2.close().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let remaining_pages = browser
        .list_targets()
        .await
        .unwrap()
        .iter()
        .filter(|t| t.target_type == "page")
        .count();

    assert_eq!(remaining_pages, initial_pages - 1);

    tab1.close().await.unwrap();
    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_list_targets() {
    let browser = launch().await;

    browser.new_tab("about:blank").await.unwrap();
    browser.new_tab("about:blank").await.unwrap();

    let targets = browser.list_targets().await.unwrap();
    let pages: Vec<_> = targets
        .iter()
        .filter(|t| t.target_type == "page")
        .collect();

    assert!(
        pages.len() >= 2,
        "expected at least 2 pages, got {}",
        pages.len()
    );

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Script injection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_inject_preload_script() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    tab.inject_preload_script("window.__test_injected = 'yes'")
        .await
        .unwrap();

    // Navigate to trigger the injected script
    tab.navigate("about:blank").await.unwrap();

    let result = tab.evaluate("window.__test_injected").await.unwrap();
    assert_eq!(result.value, serde_json::json!("yes"));

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_inject_preload_script_persists_across_navigations() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    tab.inject_preload_script("window.__counter = (window.__counter || 0) + 1")
        .await
        .unwrap();

    // Navigate twice — the injected script should run each time
    tab.navigate("about:blank").await.unwrap();
    let r1 = tab.evaluate("window.__counter").await.unwrap();
    assert_eq!(r1.value, serde_json::json!(1));

    tab.navigate("about:blank").await.unwrap();
    let r2 = tab.evaluate("window.__counter").await.unwrap();
    assert_eq!(r2.value, serde_json::json!(1)); // fresh document each time

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Window bounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_set_bounds() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    // Should not error
    tab.set_bounds(10, 20, 800, 600).await.unwrap();

    // Read back via JS — innerWidth/innerHeight may differ from outer bounds
    // due to chrome UI, but they should be non-zero and reasonable.
    let w = tab.evaluate("window.innerWidth").await.unwrap();
    let h = tab.evaluate("window.innerHeight").await.unwrap();
    assert!(
        w.value.as_f64().unwrap() > 0.0,
        "innerWidth should be positive"
    );
    assert!(
        h.value.as_f64().unwrap() > 0.0,
        "innerHeight should be positive"
    );

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Shared context — BroadcastChannel (requires real origin)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_broadcast_channel_across_tabs() {
    let base_url = start_local_server().await;
    let browser = launch().await;

    let tab1 = browser.new_tab(&base_url).await.unwrap();
    let tab2 = browser.new_tab(&base_url).await.unwrap();

    // Listener on tab2
    tab2.evaluate(
        "window._msgs = [];
         const bc = new BroadcastChannel('test');
         bc.onmessage = e => window._msgs.push(e.data);",
    )
    .await
    .unwrap();

    // Send from tab1
    tab1.evaluate("new BroadcastChannel('test').postMessage('ping')")
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let result = tab2.evaluate("window._msgs").await.unwrap();
    let msgs = result.value.as_array().expect("expected array");
    assert_eq!(msgs, &[serde_json::json!("ping")]);

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Shared context — localStorage (requires real origin)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_shared_localstorage_across_tabs() {
    let base_url = start_local_server().await;
    let browser = launch().await;

    let tab1 = browser.new_tab(&base_url).await.unwrap();
    let tab2 = browser.new_tab(&base_url).await.unwrap();

    tab1.evaluate("localStorage.setItem('k', 'from_tab1')")
        .await
        .unwrap();

    let result = tab2
        .evaluate("localStorage.getItem('k')")
        .await
        .unwrap();
    assert_eq!(result.value, serde_json::json!("from_tab1"));

    browser.close().await.unwrap();
}

// ---------------------------------------------------------------------------
// Raw CDP access
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_raw_cdp_command() {
    let browser = launch().await;

    // Send a raw Browser.getVersion via the CDP client
    let cdp = browser.cdp().clone();
    let result = cdp
        .send("Browser.getVersion", serde_json::json!({}), None)
        .await
        .unwrap();

    assert!(result.get("product").is_some(), "expected 'product' key");
    assert!(
        result.get("userAgent").is_some(),
        "expected 'userAgent' key"
    );

    browser.close().await.unwrap();
}

#[tokio::test]
async fn test_raw_cdp_target_info() {
    let browser = launch().await;
    let tab = browser.new_tab("about:blank").await.unwrap();

    // Use the session to get the frame tree via CDP
    let cdp = browser.cdp().clone();
    let result = cdp
        .send(
            "Page.getFrameTree",
            serde_json::json!({}),
            Some(tab.session_id()),
        )
        .await
        .unwrap();

    let url = result["frameTree"]["frame"]["url"]
        .as_str()
        .unwrap_or("");
    assert_eq!(url, "about:blank");

    browser.close().await.unwrap();
}
