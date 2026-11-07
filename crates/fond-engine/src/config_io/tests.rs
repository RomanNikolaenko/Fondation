use super::*;
use chrono::Utc;

#[test]
fn load_or_create_writes_defaults_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.toml");
    let c = load_or_create(&p).unwrap();
    assert_eq!(c, EngineConfig::default());
    assert!(p.is_file());
    assert_eq!(load_or_create(&p).unwrap(), c);
}

#[test]
fn save_atomic_roundtrip_and_rejects_invalid() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.toml");
    let mut c = EngineConfig::default();
    c.risk.max_open_positions = 7;
    save_atomic(&p, &c).unwrap();
    assert_eq!(load_or_create(&p).unwrap(), c);
    assert!(!tmp.path().join("config.toml.tmp").exists());

    let before = std::fs::read_to_string(&p).unwrap();
    let mut bad = c.clone();
    bad.risk.max_gross_exposure = 1.5;
    bad.api_port = 80;
    match save_atomic(&p, &bad) {
        Err(ConfigError::Invalid(m)) => assert_eq!(m.len(), 2, "{m:?}"),
        other => panic!("очікували Invalid, отримали {other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    assert!(!tmp.path().join("config.toml.tmp").exists());
}

#[test]
fn parse_error_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.toml");
    std::fs::write(&p, "api_port = [").unwrap();
    assert!(matches!(load_or_create(&p), Err(ConfigError::Parse(_))));
}

#[test]
fn invalid_loaded_config_is_reported_and_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.toml");
    std::fs::write(&p, "[risk]\nrisk_per_trade = 5.0\n").unwrap();
    match load_or_create(&p) {
        Err(ConfigError::Invalid(m)) => {
            assert_eq!(m.len(), 1, "{m:?}");
            assert!(m[0].contains("risk_per_trade"));
        }
        other => panic!("очікували Invalid, отримали {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "[risk]\nrisk_per_trade = 5.0\n"
    );
}

#[test]
fn write_discovery_is_atomic_json() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("engine.json");
    let d = Discovery {
        port: 47615,
        pid: 1234,
        started_at: Utc::now(),
    };
    write_discovery(&p, &d).unwrap();
    let back: Discovery = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(back, d);
    assert!(!tmp.path().join("engine.json.tmp").exists());
    let d2 = Discovery { pid: 99, ..d };
    write_discovery(&p, &d2).unwrap();
    let back: Discovery = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(back.pid, 99);
}
