use std::fs;
use std::path::{Path, PathBuf};

use chrono::{NaiveDate, TimeZone, Utc};
use fond_core::intent::Intent;
use fond_core::orders::BrokerOrder;
use fond_core::types::Bar;
use serde_json::json;
use ts_rs::{Config, ExportError, TS};

use super::*;
use crate::config::EngineConfig;
use crate::events::{EngineEventDto, JournalEventDto};

/// Усі типи контракту разом із залежностями.
fn export_to(cfg: &Config) -> Result<(), ExportError> {
    HealthDto::export_all(cfg)?;
    StatusDto::export_all(cfg)?;
    AccountDto::export_all(cfg)?;
    PositionDto::export_all(cfg)?;
    SignalDto::export_all(cfg)?;
    AiProposalDto::export_all(cfg)?;
    AiRunQueuedDto::export_all(cfg)?;
    ScanResultDto::export_all(cfg)?;
    UniverseEntryDto::export_all(cfg)?;
    BacktestRunDto::export_all(cfg)?;
    BacktestDetailDto::export_all(cfg)?;
    HistoryJobDto::export_all(cfg)?;
    SecretStatusDto::export_all(cfg)?;
    VerifyResultDto::export_all(cfg)?;
    KillRequest::export_all(cfg)?;
    KillResultDto::export_all(cfg)?;
    ImportResultDto::export_all(cfg)?;
    SetSecretRequest::export_all(cfg)?;
    AddSymbolRequest::export_all(cfg)?;
    ExcludeRequest::export_all(cfg)?;
    HistoryRequest::export_all(cfg)?;
    ApiErrorDto::export_all(cfg)?;
    EngineConfig::export_all(cfg)?;
    EngineEventDto::export_all(cfg)?;
    JournalEventDto::export_all(cfg)?;
    Intent::export_all(cfg)?;
    BrokerOrder::export_all(cfg)?;
    Bar::export_all(cfg)?;
    Ok(())
}

/// `pnpm gen:types`: пише прив'язки в `TS_RS_EXPORT_DIR` (`.cargo/config.toml`).
#[test]
fn export_bindings() {
    export_to(&Config::from_env()).unwrap();
}

fn ts_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            ts_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ts") {
            out.push(path);
        }
    }
}

/// serde пише 64-бітні цілі як JSON-числа, тож у TS вони мають бути `number`, а не `bigint`.
#[test]
fn bindings_have_no_bigint() {
    let dir = tempfile::tempdir().unwrap();
    export_to(&Config::new().with_out_dir(dir.path())).unwrap();
    let mut files = Vec::new();
    ts_files(dir.path(), &mut files);
    assert!(files.len() > 30, "очікувалось більше файлів: {files:?}");
    let offenders: Vec<&PathBuf> = files
        .iter()
        .filter(|p| fs::read_to_string(p).unwrap().contains("bigint"))
        .collect();
    assert!(offenders.is_empty(), "bigint у {offenders:?}");
    // Ключові типи справді експортовано.
    for name in [
        "StatusDto.ts",
        "EngineEventDto.ts",
        "BacktestResult.ts",
        "Intent.ts",
    ] {
        assert!(
            files
                .iter()
                .any(|p| p.file_name().is_some_and(|f| f == name)),
            "немає {name}"
        );
    }
}

#[test]
fn requests_parse_from_snake_case_json() {
    let k: KillRequest = serde_json::from_value(json!({ "close_positions": true })).unwrap();
    assert!(k.close_positions);
    let h: HistoryRequest = serde_json::from_value(json!({ "from": "2016-01-01" })).unwrap();
    assert_eq!(h.from, NaiveDate::from_ymd_opt(2016, 1, 1).unwrap());
    let e: ExcludeRequest = serde_json::from_value(json!({ "excluded": false })).unwrap();
    assert!(!e.excluded);
    let s: SetSecretRequest = serde_json::from_value(json!({ "value": "x" })).unwrap();
    assert_eq!(s.value, "x");
}

#[test]
fn status_and_error_shapes() {
    let s = StatusDto {
        engine_mode: EngineModeDto {
            state: "paused".into(),
            reason: Some("x".into()),
        },
        account_mode: fond_core::types::AccountMode::Paper,
        execution_mode: fond_core::types::ExecutionMode::Approval,
        needs_setup: vec!["alpaca_key_id".into()],
        next_step: Some(NextStepDto {
            name: "analysis".into(),
            at: Utc.with_ymd_and_hms(2026, 11, 4, 21, 30, 0).unwrap(),
        }),
        last_reconcile_at: None,
        version: "0.1.0".into(),
        now: Utc.with_ymd_and_hms(2026, 11, 4, 20, 0, 0).unwrap(),
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["engine_mode"]["state"], "paused");
    assert_eq!(v["account_mode"], "paper");
    assert_eq!(v["execution_mode"], "approval");
    assert_eq!(v["next_step"]["at"], "2026-11-04T21:30:00Z");
    assert_eq!(serde_json::from_value::<StatusDto>(v).unwrap(), s);

    let e = ApiErrorDto {
        error: "Некоректна конфігурація".into(),
        details: vec!["a".into()],
    };
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({ "error": "Некоректна конфігурація", "details": ["a"] })
    );
}

#[test]
fn kill_and_import_results_shape() {
    let k = KillResultDto {
        broker_side: "scheduled".into(),
        message: "m".into(),
    };
    assert_eq!(
        serde_json::to_value(&k).unwrap(),
        json!({ "broker_side": "scheduled", "message": "m" })
    );
    let i = ImportResultDto {
        added: 2,
        skipped: 1,
    };
    assert_eq!(
        serde_json::to_value(&i).unwrap(),
        json!({ "added": 2, "skipped": 1 })
    );
    let ts = ImportResultDto::decl(&Config::new());
    assert!(ts.contains("added: number"), "{ts}");
}

#[test]
fn job_events_are_tagged() {
    let e = EngineEventDto::HistoryProgress {
        job: HistoryJobDto {
            status: "running".into(),
            progress: 0.5,
            message: "m".into(),
        },
    };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], "history_progress");
    assert_eq!(v["job"]["status"], "running");
    let b = EngineEventDto::BacktestStatus {
        id: "bt-1".into(),
        status: "done".into(),
    };
    assert_eq!(
        serde_json::to_value(&b).unwrap(),
        json!({ "type": "backtest_status", "id": "bt-1", "status": "done" })
    );
}
