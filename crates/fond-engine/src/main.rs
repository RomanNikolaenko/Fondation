mod config_io;
mod lock;
mod logging;
mod paths;
mod schedule;

use std::path::PathBuf;

use chrono::Utc;
use clap::Parser;

use crate::lock::{InstanceLock, LockError};
use crate::paths::DataDir;

/// Торговий рушій Fond.
#[derive(Debug, Parser)]
#[command(name = "fond-engine", version)]
struct Cli {
    /// Каталог даних (типово `<config_dir>/Fond`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let dir = match DataDir::resolve(cli.data_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не вдалося підготувати каталог даних: {e}");
            std::process::exit(1);
        }
    };

    // Блокування тримається до кінця main.
    let _lock = match InstanceLock::acquire(&dir.lock()) {
        Ok(l) => l,
        Err(LockError::AlreadyRunning) => {
            eprintln!("fond-engine вже запущено");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let _log_guard = logging::init_logging(&dir.logs());
    match logging::prune_logs(&dir.logs(), 30, Utc::now()) {
        Ok(n) if n > 0 => tracing::info!(removed = n, "видалено старі лог-файли"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "не вдалося почистити логи"),
    }

    let cfg = match config_io::load_or_create(&dir.config()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %dir.root.display(),
        execution_mode = ?cfg.execution_mode,
        api_port = cfg.api_port,
        "fond-engine запущено"
    );

    // Запуск runtime додається в Task 22.
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "не вдалося очікувати Ctrl+C");
    }
    tracing::info!("fond-engine зупинено");
}
