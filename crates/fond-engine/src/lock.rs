//! Захист від двох одночасно запущених рушіїв: ексклюзивне блокування файлу.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs4::{FileExt, TryLockError};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("fond-engine вже запущено")]
    AlreadyRunning,
    #[error("помилка файлу блокування: {0}")]
    Io(#[from] io::Error),
}

/// Тримає блокування, доки живе значення (файл закривається разом із ним).
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(LockError::AlreadyRunning),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests;
