//! Читання й атомарний запис `config.toml` та `engine.json`.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use fond_api::config::{Discovery, EngineConfig};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("помилка файлу конфігурації: {0}")]
    Io(#[from] io::Error),
    #[error("не вдалося розібрати конфігурацію: {0}")]
    Parse(String),
    #[error("некоректна конфігурація: {}", .0.join("; "))]
    Invalid(Vec<String>),
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name: OsString = path.file_name().unwrap_or_default().to_owned();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Пише у `<path>.tmp`, скидає на диск і перейменовує поверх цілі.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path(path);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Файлу немає: записує `EngineConfig::default()` і повертає його.
pub fn load_or_create(path: &Path) -> Result<EngineConfig, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let cfg = EngineConfig::default();
            save_atomic(path, &cfg)?;
            return Ok(cfg);
        }
        Err(e) => return Err(e.into()),
    };
    let cfg: EngineConfig = toml::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    cfg.validate().map_err(ConfigError::Invalid)?;
    Ok(cfg)
}

/// Спершу `validate` (файл не чіпається при помилці), далі tmp + rename.
pub fn save_atomic(path: &Path, cfg: &EngineConfig) -> Result<(), ConfigError> {
    cfg.validate().map_err(ConfigError::Invalid)?;
    let text = toml::to_string_pretty(cfg).map_err(|e| ConfigError::Parse(e.to_string()))?;
    write_atomic(path, text.as_bytes())?;
    Ok(())
}

#[allow(dead_code, reason = "викликається з runtime у Task 22")]
pub fn write_discovery(path: &Path, d: &Discovery) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(d).map_err(io::Error::other)?;
    write_atomic(path, &json)
}

#[cfg(test)]
mod tests;
