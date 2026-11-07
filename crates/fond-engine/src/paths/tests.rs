use super::*;

#[test]
fn data_dir_override_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("custom");
    let d = DataDir::resolve(Some(root.clone())).unwrap();
    assert_eq!(d.root, root);
    assert!(root.is_dir(), "каталог має бути створено");
    assert_eq!(d.db(), root.join("fond.db"));
    assert_eq!(d.config(), root.join("config.toml"));
    assert_eq!(d.logs(), root.join("logs"));
    assert_eq!(d.lock(), root.join("engine.lock"));
    assert_eq!(d.discovery(), root.join("engine.json"));
}

#[test]
fn default_data_dir_ends_with_fond() {
    // Без створення каталогу: лише перевірка імені.
    let d = DataDir::default_root().unwrap();
    assert_eq!(d.file_name().unwrap(), "Fond");
}
