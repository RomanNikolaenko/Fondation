//! Логування у файл (щодня новий) і в stderr; чистка старих файлів.

use std::io;
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const LOG_PREFIX: &str = "engine.log";

/// Фільтр із `FOND_LOG` (типово `info`). Повернутий guard треба тримати до кінця роботи.
pub fn init_logging(logs: &Path) -> WorkerGuard {
    let _ = std::fs::create_dir_all(logs);
    let appender = tracing_appender::rolling::daily(logs, LOG_PREFIX);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("FOND_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(fmt::layer().with_writer(io::stderr))
        .try_init();
    guard
}

/// Дата з імені `engine.log.YYYY-MM-DD`; інші імена повертають `None`.
fn log_file_date(name: &str) -> Option<NaiveDate> {
    let rest = name.strip_prefix(LOG_PREFIX)?.strip_prefix('.')?;
    NaiveDate::parse_from_str(rest, "%Y-%m-%d").ok()
}

/// Видаляє `engine.log.YYYY-MM-DD` із датою раніше за `now - keep_days`; повертає кількість.
pub fn prune_logs(dir: &Path, keep_days: i64, now: DateTime<Utc>) -> io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let cutoff = now.date_naive() - Duration::days(keep_days);
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(date) = name.to_str().and_then(log_file_date) else {
            continue;
        };
        if date < cutoff && entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests;
