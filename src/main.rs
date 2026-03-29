use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use chromerunner::{BrowserApi, TabApi};

#[derive(Parser)]
#[command(name = "chromerunner", about = "Browser automation via CDP / BiDi / WebDriver")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Run in headless mode (not supported by Safari)
    #[arg(long, global = true)]
    headless: bool,

    /// Browser backend to use
    #[arg(long, global = true, default_value = "chrome")]
    browser: BrowserKind,

    /// Enable trace output (debug level). Use RUST_LOG=chromerunner=trace for full message bodies.
    #[arg(short = 't', long, global = true)]
    trace: bool,
}

#[derive(Clone, ValueEnum)]
enum BrowserKind {
    #[cfg(feature = "chrome")]
    Chrome,
    #[cfg(feature = "firefox")]
    Firefox,
    #[cfg(feature = "safari")]
    Safari,
}

#[derive(Subcommand)]
enum Commands {
    /// Print browser version info
    Version,
    /// Navigate to a URL and print page title
    Navigate { url: String },
    /// Evaluate a JavaScript expression
    Eval {
        expression: String,
        #[arg(long)]
        url: Option<String>,
    },
    /// List all browser targets
    Targets,
    /// Run a demo exercising major features
    Demo {
        #[arg(long)]
        pause: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.trace {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("chromerunner=debug"));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .init();
    }

    match cli.browser {
        #[cfg(feature = "chrome")]
        BrowserKind::Chrome => run::<chromerunner::chrome::Browser>(cli).await,
        #[cfg(feature = "firefox")]
        BrowserKind::Firefox => run::<chromerunner::firefox::Browser>(cli).await,
        #[cfg(feature = "safari")]
        BrowserKind::Safari => run::<chromerunner::safari::Browser>(cli).await,
    }
}

async fn run<B: BrowserApi>(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Version => {
            let browser = B::launch(cli.headless).await?;
            let v = browser.version().await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
            browser.close().await?;
        }
        Commands::Navigate { url } => {
            let browser = B::launch(cli.headless).await?;
            let tab = browser.new_tab("about:blank").await?;
            tab.navigate(&url).await?;
            println!("Title: {}", tab.title().await?);
            println!("URL:   {}", tab.url().await?);
            browser.close().await?;
        }
        Commands::Eval { expression, url } => {
            let browser = B::launch(cli.headless).await?;
            let tab = browser.new_tab("about:blank").await?;
            if let Some(url) = url {
                tab.navigate(&url).await?;
            }
            let result = tab.evaluate(&expression).await?;
            println!("Type:  {}", result.result_type);
            println!("Value: {}", result.value);
            browser.close().await?;
        }
        Commands::Targets => {
            let browser = B::launch(cli.headless).await?;
            for t in browser.list_targets().await? {
                println!(
                    "[{}] {} - {} ({})",
                    t.target_type, t.target_id, t.title, t.url
                );
            }
            browser.close().await?;
        }
        Commands::Demo { pause } => {
            run_demo::<B>(cli.headless, pause).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Demo
// ---------------------------------------------------------------------------

async fn run_demo<B: BrowserApi>(headless: bool, pause: bool) -> anyhow::Result<()> {
    println!("=== Browser Demo ===\n");

    // 0. Start a tiny local HTTP server so windows share a real origin
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_port = listener.local_addr()?.port();
    let base_url = format!("http://127.0.0.1:{}", server_port);
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

    // 1. Launch
    println!("1. Launching browser...");
    let browser = B::launch(headless).await?;
    let version = browser.version().await?;
    println!("   Version: {}", serde_json::to_string_pretty(&version)?);

    // 2. Open 4 windows tiled 2x2 with colored backgrounds
    println!("\n2. Opening 4 tiled windows (2x2)...");
    let colors = [
        ("#e74c3c", "Red"),
        ("#3498db", "Blue"),
        ("#2ecc71", "Green"),
        ("#f39c12", "Orange"),
    ];
    let tile_w = 640;
    let tile_h = 480;
    let positions = [(0, 0), (tile_w, 0), (0, tile_h), (tile_w, tile_h)];

    let mut windows = Vec::new();
    for (i, ((color, name), (x, y))) in colors.iter().zip(positions.iter()).enumerate() {
        let win = browser.new_window(&base_url).await?;
        win.set_bounds(*x, *y, tile_w, tile_h).await?;
        win.evaluate(&format!(
            r#"document.title = 'Window {i} - {name}';
               document.body.style.cssText = 'margin:0;display:flex;align-items:center;\
               justify-content:center;background:{color};font-family:sans-serif;height:100vh';
               document.body.innerHTML = '<h1 style="color:white;font-size:3em;text-align:center">\
               Window {i}<br>{name}</h1>';"#
        ))
        .await?;
        println!(
            "   [{}] Window {} - {} at ({}, {}) {}x{}",
            i, i, name, x, y, tile_w, tile_h
        );
        windows.push(win);
    }

    // 3. Pause if requested
    if pause {
        println!("\n   Press Enter to continue...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
    }

    // 4. Cross-window messaging via BroadcastChannel (shared origin)
    println!("\n3. Cross-window messaging (BroadcastChannel)...");
    for (i, win) in windows.iter().enumerate().skip(1) {
        win.evaluate(&format!(
            r#"window._received = [];
               const bc = new BroadcastChannel('demo');
               bc.onmessage = e => {{
                   window._received.push(e.data);
                   document.querySelector('h1').innerHTML =
                       'Window {i}<br>Got: ' + e.data;
               }};"#
        ))
        .await?;
    }
    windows[0]
        .evaluate("new BroadcastChannel('demo').postMessage('hello from window 0')")
        .await?;
    println!("   Window 0 sent: 'hello from window 0'");

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    for (i, win) in windows.iter().enumerate().skip(1) {
        let result = win.evaluate("window._received").await?;
        println!("   Window {} received: {}", i, result.value);
    }

    // Pause after messaging
    if pause {
        println!("\n   Press Enter to continue...");
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
    }

    // 5. Shared localStorage across windows (same origin)
    println!("\n4. Shared localStorage...");
    windows[0]
        .evaluate("localStorage.setItem('demo_key', 'set by window 0')")
        .await?;
    println!("   Window 0 set localStorage['demo_key'] = 'set by window 0'");
    let result = windows[2]
        .evaluate("localStorage.getItem('demo_key')")
        .await?;
    println!(
        "   Window 2 read localStorage['demo_key'] = {}",
        result.value
    );

    // 6. Evaluate JS in each window
    println!("\n5. Evaluating JavaScript in each window...");
    for (i, win) in windows.iter().enumerate() {
        let result = win
            .evaluate(&format!("'Hello from window {}'", i))
            .await?;
        println!("   [{}] {}", i, result.value);
    }

    // 7. Navigate and check URL
    println!("\n6. Navigate window 0...");
    windows[0].navigate(&base_url).await?;
    let url = windows[0].url().await?;
    println!("   URL: {}", url);

    // 8. List targets
    println!("\n7. Listing all targets...");
    let targets = browser.list_targets().await?;
    for t in &targets {
        println!("   [{}] {} - {}", t.target_type, t.target_id, t.url);
    }

    // 9. Close windows
    println!("\n8. Closing windows...");
    for (i, win) in windows.into_iter().enumerate() {
        let id = win.target_id().to_string();
        win.close().await?;
        println!("   Closed window {} ({})", i, id);
    }

    // 10. Final target count
    let targets = browser.list_targets().await?;
    println!("\n9. Remaining targets: {}", targets.len());

    println!("\n=== Demo complete ===");
    browser.close().await?;
    Ok(())
}
