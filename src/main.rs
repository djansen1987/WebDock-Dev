//! WebDock Dev - single-binary dev environment manager.
//!
//! Normal mode:  `webdock-dev`                                  (tray icon + UI on 127.0.0.1:9000)
//! Helper mode:  `webdock-dev --hosts-helper <staged> <target>` (run elevated, writes the hosts file)
//! `--background` starts without opening the browser (used by "Start with Windows").

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod backup;
mod cert;
mod config;
mod docker;
mod error;
mod git;
mod hosts;
mod jobs;
mod proc;
mod projects;
#[cfg(windows)]
mod tray;
mod updates;

use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--hosts-helper") {
        return match (args.get(2), args.get(3)) {
            (Some(s), Some(t)) => {
                let res = hosts::run_helper(s.as_ref(), t.as_ref());
                // The helper has no console; hand the reason back through a file next to the staged copy.
                if let Err(e) = &res {
                    let _ = std::fs::write(format!("{s}.err"), format!("{e:#}"));
                }
                res
            }
            _ => anyhow::bail!("usage: --hosts-helper <staged> <target>"),
        };
    }
    let background = args.iter().any(|a| a == "--background");

    let cfg = Arc::new(config::Config::from_env());
    init_logging(&cfg);

    // A second instance only opens the dashboard of the running one.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(_) => {
            proc::open_url(&format!("http://localhost:{}", cfg.port));
            return Ok(());
        }
    };
    listener.set_nonblocking(true)?;

    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let port = cfg.port;
    let server = std::thread::spawn(move || -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        rt.block_on(async move {
            let state = api::AppState {
                cfg,
                docker: docker::DockerClient::connect()?,
                lock: Arc::new(tokio::sync::Mutex::new(())),
                jobs: jobs::Jobs::default(),
            };
            tokio::spawn(updates::scheduler(state.cfg.clone(), state.docker.clone()));
            let listener = tokio::net::TcpListener::from_std(listener)?;
            tracing::info!("WebDock Dev on http://localhost:{port}");
            let shutdown = async move {
                let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
            };
            axum::serve(listener, api::router(state)).with_graceful_shutdown(shutdown).await?;
            Ok(())
        })
    });

    if !background {
        proc::open_url(&format!("http://localhost:{port}"));
    }

    #[cfg(windows)]
    {
        let _ = &server;
        tray::run(port, stop_tx);
    }

    #[cfg(not(windows))]
    {
        let _ = stop_tx;
        server.join().map_err(|_| anyhow::anyhow!("server thread panicked"))?
    }
}

/// Console in debug builds; `<data>/portal.log` in release (the GUI build has no console).
fn init_logging(cfg: &config::Config) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if cfg!(debug_assertions) {
        builder.init();
    } else if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(cfg.data_dir.join("portal.log")) {
        builder.with_ansi(false).with_writer(std::sync::Mutex::new(f)).init();
    }
}
