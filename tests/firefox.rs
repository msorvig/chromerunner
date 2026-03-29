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
    assert!(targets.len() >= 2, "expected >=2 targets, got {}", targets.len());

    browser.close().await.unwrap();
}
