//! Автозапуск рушія й застосунку через `HKCU\...\Run`.

use std::io;
use std::path::Path;

/// Гілка реєстру, яку читає Windows під час входу користувача.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

const ENGINE_VALUE: &str = "FondEngine";
const DESKTOP_VALUE: &str = "FondDesktop";

/// Команда запуску: шлях у лапках і аргументи через пробіл.
pub fn run_value(exe: &Path, args: &[&str]) -> String {
    let mut value = format!("\"{}\"", exe.display());
    for arg in args {
        value.push(' ');
        value.push_str(arg);
    }
    value
}

pub fn get_autostart() -> io::Result<bool> {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_READ)?;
        let engine = key.get_value::<String, _>(ENGINE_VALUE).is_ok();
        let desktop = key.get_value::<String, _>(DESKTOP_VALUE).is_ok();
        Ok(engine && desktop)
    }
    #[cfg(not(windows))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "автозапуск підтримується лише у Windows",
        ))
    }
}

pub fn set_autostart(enabled: bool) -> io::Result<()> {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)?;

        if enabled {
            let engine = crate::sidecar::engine_exe_path()?;
            if !engine.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("не знайдено {}", engine.display()),
                ));
            }
            let desktop = std::env::current_exe()?;
            key.set_value(ENGINE_VALUE, &run_value(&engine, &[]))?;
            key.set_value(DESKTOP_VALUE, &run_value(&desktop, &["--minimized"]))?;
        } else {
            // Відсутнє значення — не помилка.
            for name in [ENGINE_VALUE, DESKTOP_VALUE] {
                match key.delete_value(name) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "автозапуск підтримується лише у Windows",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_value_quotes_path_with_spaces() {
        assert_eq!(
            run_value(
                Path::new(r"C:\Program Files\Fond\fond-desktop.exe"),
                &["--minimized"]
            ),
            r#""C:\Program Files\Fond\fond-desktop.exe" --minimized"#
        );
    }

    #[test]
    fn run_value_without_args_is_just_quoted_path() {
        assert_eq!(
            run_value(Path::new(r"C:\Fond\fond-engine.exe"), &[]),
            r#""C:\Fond\fond-engine.exe""#
        );
    }
}
