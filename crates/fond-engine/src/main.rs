mod api;
mod config_io;
mod cycle;
mod deps;
mod events;
mod health;
mod jobs;
mod lock;
mod logging;
mod paths;
mod runtime;
mod schedule;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use clap::Parser;
use fond_adapters::secrets::{KeyringStore, SecretStore};
use fond_api::config::Discovery;
use fond_core::traits::{Clock, SystemClock};
use fond_store::Store;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::api::auth::{bind_addr, ensure_api_token};
use crate::api::{ApiState, router};
use crate::config_io::write_discovery;
use crate::deps::{LiveDepsFactory, SetupError};
use crate::events::EventBus;
use crate::health::BrokerHealth;
use crate::lock::{InstanceLock, LockError};
use crate::paths::DataDir;
use crate::runtime::Engine;

/// Розмір буфера шини подій.
const BUS_CAPACITY: usize = 1024;
/// Скільки чекати фонові завдання (бектест у `spawn_blocking`) після зупинки.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Торговий рушій Fond.
#[derive(Debug, Parser)]
#[command(name = "fond-engine", version)]
struct Cli {
    /// Каталог даних (типово `<config_dir>/Fond`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("не вдалося запустити tokio: {e}");
            return ExitCode::from(1);
        }
    };
    let code = rt.block_on(run(cli));
    rt.shutdown_timeout(SHUTDOWN_GRACE);
    code
}

async fn run(cli: Cli) -> ExitCode {
    let dir = match DataDir::resolve(cli.data_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не вдалося підготувати каталог даних: {e}");
            return ExitCode::from(1);
        }
    };

    // Блокування тримається до кінця run.
    let _lock = match InstanceLock::acquire(&dir.lock()) {
        Ok(l) => l,
        Err(LockError::AlreadyRunning) => {
            eprintln!("fond-engine вже запущено");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
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
            return ExitCode::from(1);
        }
    };

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %dir.root.display(),
        execution_mode = ?cfg.execution_mode,
        api_port = cfg.api_port,
        "fond-engine запущено"
    );

    let store = match Store::open(&dir.db()).await {
        Ok(s) => s,
        Err(e) => return fail(format!("не вдалося відкрити базу даних: {e}")),
    };
    match store.seed_universe_if_empty().await {
        Ok(true) => tracing::info!("список тікерів заповнено вбудованими S&P 500 і ETF"),
        Ok(false) => {}
        Err(e) => return fail(format!("не вдалося заповнити список тікерів: {e}")),
    }

    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringStore::new());
    let token = match ensure_api_token(secrets.as_ref()).await {
        Ok(t) => t,
        Err(e) => return fail(format!("не вдалося отримати токен API: {e}")),
    };

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let bus = EventBus::new(BUS_CAPACITY);
    let factory = Arc::new(LiveDepsFactory {
        secrets: secrets.clone(),
        store: store.clone(),
        clock: clock.clone(),
        bus: bus.clone(),
        health: Arc::new(BrokerHealth::default()),
    });
    let engine = Engine::new(
        factory,
        store,
        clock,
        bus,
        cfg.clone(),
        dir.config(),
        secrets,
    );
    if let Err(SetupError::MissingSecrets(keys)) = engine.deps().await {
        tracing::warn!(
            ?keys,
            "рушій не налаштовано: бракує ключів, кроки циклу не виконуються"
        );
    }

    let addr = bind_addr(cfg.api_port);
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => return fail(format!("не вдалося відкрити порт API {addr}: {e}")),
    };
    let port = listener.local_addr().map_or(cfg.api_port, |a| a.port());
    let discovery = Discovery {
        port,
        pid: std::process::id(),
        started_at: Utc::now(),
    };
    if let Err(e) = write_discovery(&dir.discovery(), &discovery) {
        return fail(format!("не вдалося записати engine.json: {e}"));
    }
    tracing::info!(%addr, "API слухає");

    let (shutdown, stop) = watch::channel(false);
    let ctrl_c = shutdown.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => tracing::info!("отримано Ctrl+C, зупинка"),
            Err(e) => tracing::error!(error = %e, "не вдалося очікувати Ctrl+C"),
        }
        let _ = ctrl_c.send(true);
    });

    let runner = tokio::spawn(engine.clone().run(stop.clone()));
    let app = router(ApiState::new(engine, token, stop.clone()));
    let mut server_stop = stop;
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_stop.wait_for(|s| *s).await;
        })
        .await;
    if let Err(e) = served {
        tracing::error!(error = %e, "API-сервер зупинився з помилкою");
    }
    // Сервер міг зупинитись і сам (помилка): тоді зупиняється й цикл.
    let _ = shutdown.send(true);
    if let Err(e) = runner.await {
        tracing::error!(error = %e, "цикл рушія аварійно перервано");
    }
    if let Err(e) = std::fs::remove_file(dir.discovery()) {
        tracing::warn!(error = %e, "не вдалося видалити engine.json");
    }
    tracing::info!("fond-engine зупинено");
    ExitCode::SUCCESS
}

/// Фатальна помилка запуску: у stderr і в лог, код виходу 1.
fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    tracing::error!("{message}");
    ExitCode::from(1)
}
