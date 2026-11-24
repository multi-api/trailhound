mod admin;
mod api;
mod error;
mod platform;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use platform::Platform;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

fn setting(flag: &str, envs: &[&str]) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == flag) {
        if let Some(v) = args.get(i + 1) {
            return Some(v.clone());
        }
    }
    envs.iter().find_map(|e| std::env::var(e).ok()).filter(|v| !v.is_empty())
}

fn main() {
    // Before any thread exists, so every arena follows the same rules.
    platform::tune_allocator();
    // Searches run on blocking threads; past a few per core they only compete.
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    tokio::runtime::Builder::new_multi_thread().enable_all().max_blocking_threads((cores * 4).clamp(16, 128)).build().expect("tokio runtime").block_on(serve());
}

async fn serve() {
    let data_dir = PathBuf::from(setting("--data-dir", &["TRAILHOUND_DATA_DIR"]).unwrap_or_else(|| "data".into()));
    let addr = setting("--addr", &["TRAILHOUND_ADDR", "MEILI_HTTP_ADDR"]).unwrap_or_else(|| "127.0.0.1:7700".into());
    let admin_key = setting("--admin-key", &["TRAILHOUND_ADMIN_KEY"]);
    let master_key = setting("--master-key", &["TRAILHOUND_MASTER_KEY", "MEILI_MASTER_KEY"]);
    let number = |flag: &str, env: &str| setting(flag, &[env]).and_then(|v| v.parse::<usize>().ok());
    trailhound_engine::configure_indexing(number("--indexing-threads", "TRAILHOUND_INDEXING_THREADS"), number("--indexing-memory-mb", "TRAILHOUND_INDEXING_MEMORY_MB").map(|mb| mb << 20));
    let generated = admin_key.is_none() && !data_dir.join("platform.json").exists();

    let platform = match Platform::open(&data_dir, admin_key, master_key) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[trailhound] cannot open data directory {}: {}", data_dir.display(), e.message);
            std::process::exit(1);
        }
    };

    let app = api::router()
        .merge(admin::router())
        .fallback(|| async { error::ApiError::new(StatusCode::NOT_FOUND, "not_found", "The requested resource was not found.") })
        .layer(DefaultBodyLimit::max(256 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .with_state(platform.clone());

    tokio::spawn(async {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            if platform::SEARCHED.swap(false, std::sync::atomic::Ordering::Relaxed) {
                let _ = tokio::task::spawn_blocking(platform::release_memory).await;
            }
        }
    });

    let flusher = platform.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            let p = flusher.clone();
            let _ = tokio::task::spawn_blocking(move || {
                p.flush_usage();
                platform::release_memory();
            })
            .await;
        }
    });

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[trailhound] cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("trailhound {} listening on http://{addr}", env!("CARGO_PKG_VERSION"));
    println!("  data directory: {}", data_dir.display());
    println!("  tenants loaded: {}", platform.tenants.read().unwrap().len());
    println!("  admin panel:    http://{addr}/_admin");
    if generated {
        println!("  admin key (generated, stored in platform.json): {}", platform.admin_key);
    }

    let on_exit: Arc<Platform> = platform.clone();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
        eprintln!("[trailhound] server error: {e}");
    }
    on_exit.flush_usage();
}
