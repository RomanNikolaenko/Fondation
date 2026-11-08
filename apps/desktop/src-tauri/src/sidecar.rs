//! Пошук, запуск і підключення до фонового `fond-engine`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fond_api::config::Discovery;
use fond_api::dto::{HealthDto, KEYRING_SERVICE, SecretKey};

use crate::engine_client::EngineClient;

/// Скільки чекати, доки рушій підніме API.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_EVERY: Duration = Duration::from_millis(250);

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// `<config_dir>/Fond` — той самий каталог, що використовує рушій.
pub fn data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Fond")
}

pub fn read_discovery(dir: &Path) -> io::Result<Discovery> {
    let raw = std::fs::read_to_string(dir.join("engine.json"))?;
    serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Токен API з Windows Credential Manager (той самий запис, що створює рушій).
pub fn read_token() -> Result<String, String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, SecretKey::EngineApiToken.account())
        .map_err(|e| format!("сховище секретів недоступне: {e}"))?;
    entry
        .get_password()
        .map_err(|e| format!("токен рушія не знайдено: {e}"))
}

/// `fond-engine.exe` поряд із застосунком (у dev — `target/debug`).
pub fn engine_exe_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "не вдалося визначити каталог застосунку",
        )
    })?;
    Ok(dir.join(if cfg!(windows) {
        "fond-engine.exe"
    } else {
        "fond-engine"
    }))
}

async fn try_connect(dir: &Path) -> Option<EngineClient> {
    let discovery = read_discovery(dir).ok()?;
    let token = read_token().ok()?;
    let client = EngineClient::new(discovery.port, token);
    match client.get::<HealthDto>("/health").await {
        Ok(health) if health.ok => Some(client),
        _ => None,
    }
}

/// Підключитися до вже запущеного рушія або запустити його у фоні.
pub async fn ensure_engine(dir: &Path) -> Result<EngineClient, String> {
    if let Some(client) = try_connect(dir).await {
        return Ok(client);
    }

    let exe = engine_exe_path().map_err(|e| format!("не вдалося знайти рушій: {e}"))?;
    if !exe.is_file() {
        return Err(format!(
            "не знайдено {} — зберіть рушій (`cargo build -p fond-engine`)",
            exe.display()
        ));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("не вдалося створити {}: {e}", dir.display()))?;

    let mut command = Command::new(&exe);
    command
        .arg("--data-dir")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
        .spawn()
        .map_err(|e| format!("не вдалося запустити {}: {e}", exe.display()))?;

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(client) = try_connect(dir).await {
            return Ok(client);
        }
        tokio::time::sleep(POLL_EVERY).await;
    }

    Err(format!(
        "рушій не відповів за {} с; логи: {}",
        STARTUP_TIMEOUT.as_secs(),
        dir.join("logs").display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_discovery_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("engine.json"),
            r#"{"port":47615,"pid":42,"started_at":"2026-11-05T14:00:00Z"}"#,
        )
        .unwrap();

        let d = read_discovery(dir.path()).unwrap();
        assert_eq!(d.port, 47615);
        assert_eq!(d.pid, 42);
    }

    #[test]
    fn read_discovery_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("engine.json"), "not json").unwrap();
        assert!(read_discovery(dir.path()).is_err());
    }

    #[test]
    fn engine_exe_sits_next_to_desktop() {
        let exe = engine_exe_path().unwrap();
        assert_eq!(exe.file_name().unwrap(), "fond-engine.exe");
        assert_eq!(exe.parent(), std::env::current_exe().unwrap().parent());
    }
}
