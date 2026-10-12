//! `scout-share`: Scout's self-hostable share server (M11.6, ADR 0021).
//!
//! ```text
//! scout-share serve [--config FILE]
//! scout-share token add NAME [--config FILE]     prints the new token once
//! scout-share token list [--config FILE]
//! scout-share token remove NAME [--config FILE]
//! ```
//! Settings: the TOML file (`--config`, or `SCOUT_SHARE_CONFIG`) and `SCOUT_SHARE_*`
//! environment variables; see the README.

use std::path::PathBuf;
use std::process::ExitCode;

use klip_share_server::config::Config;
use klip_share_server::store::Store;
use klip_share_server::{AppState, serve};

const USAGE: &str = "usage:
  scout-share serve [--config FILE]
  scout-share token add NAME [--config FILE]
  scout-share token list [--config FILE]
  scout-share token remove NAME [--config FILE]
settings: SCOUT_SHARE_PUBLIC_URL (needed to serve), SCOUT_SHARE_DATA_DIR, SCOUT_SHARE_LISTEN, … (README)";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut config: Option<PathBuf> = std::env::var_os("SCOUT_SHARE_CONFIG").map(PathBuf::from);
    if let Some(i) = args.iter().position(|a| a == "--config") {
        let Some(path) = args.get(i.saturating_add(1)).cloned() else {
            eprintln!("--config needs a file\n{USAGE}");
            return ExitCode::from(2);
        };
        config = Some(PathBuf::from(path));
        args.drain(i..i.saturating_add(2).min(args.len()));
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if matches!(words.as_slice(), ["--version"] | ["-V"]) {
        println!("scout-share {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let cfg = match Config::load(config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("scout-share: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match words.as_slice() {
        ["serve"] => run_server(cfg),
        ["token", "add", name] => token_add(&cfg, name),
        ["token", "list"] => token_list(&cfg),
        ["token", "remove", name] => token_remove(&cfg, name),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scout-share: {e}");
            ExitCode::FAILURE
        }
    }
}

fn store(cfg: &Config) -> Result<Store, String> {
    Store::open(&cfg.data_dir).map_err(|e| format!("{}: {e}", cfg.data_dir.display()))
}

fn token_add(cfg: &Config, name: &str) -> Result<(), String> {
    let token = store(cfg)?.add_token(name).map_err(|e| e.to_string())?;
    println!("{token}");
    eprintln!("Token \"{name}\" made. Put it in Scout (Settings → Sharing); it isn't shown again.");
    Ok(())
}

fn token_list(cfg: &Config) -> Result<(), String> {
    let t = store(cfg)?.tokens().map_err(|e| e.to_string())?;
    if t.tokens.is_empty() {
        println!("no tokens; make one with: scout-share token add NAME");
    }
    for e in t.tokens {
        println!("{}\t(made {})", e.name, e.created_unix);
    }
    Ok(())
}

fn token_remove(cfg: &Config, name: &str) -> Result<(), String> {
    if store(cfg)?.remove_token(name).map_err(|e| e.to_string())? {
        println!("removed {name}");
        Ok(())
    } else {
        Err(format!("no token called {name}"))
    }
}

fn run_server(mut cfg: Config) -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    cfg.check_public_url().map_err(|e| e.to_string())?;
    let addr = cfg.listen_addr().map_err(|e| e.to_string())?;
    let state = AppState::new(cfg).map_err(|e| e.to_string())?;
    if state
        .store
        .tokens()
        .map(|t| t.tokens.is_empty())
        .unwrap_or(true)
    {
        tracing::warn!("no upload tokens yet: make one with `scout-share token add NAME`");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("no runtime: {e}"))?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("can't listen on {addr}: {e}"))?;
        tracing::info!(%addr, public_url = %state.cfg.public_url, "scout-share serving");
        serve(listener, state, shutdown_signal())
            .await
            .map_err(|e| e.to_string())
    })
}

/// Ctrl+C, or SIGTERM (`docker stop`).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let term = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut s) => {
                    s.recv().await;
                }
                Err(_) => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = ctrl_c => {},
            () = term => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
    tracing::info!("shutting down");
}
