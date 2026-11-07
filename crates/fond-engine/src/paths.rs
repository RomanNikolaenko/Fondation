//! Розташування файлів рушія в каталозі даних.

use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    pub root: PathBuf,
}

#[allow(
    dead_code,
    reason = "db() і discovery() використовує runtime у Task 22"
)]
impl DataDir {
    /// Типовий каталог: `<config_dir>/Fond`.
    pub fn default_root() -> io::Result<PathBuf> {
        dirs::config_dir().map(|d| d.join("Fond")).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "не вдалося визначити каталог конфігурації",
            )
        })
    }

    /// Визначає каталог даних (`override_dir` або типовий) і створює його.
    pub fn resolve(override_dir: Option<PathBuf>) -> io::Result<Self> {
        let root = match override_dir {
            Some(p) => p,
            None => Self::default_root()?,
        };
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("fond.db")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn lock(&self) -> PathBuf {
        self.root.join("engine.lock")
    }

    pub fn discovery(&self) -> PathBuf {
        self.root.join("engine.json")
    }
}

#[cfg(test)]
mod tests;
