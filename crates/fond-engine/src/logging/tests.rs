use super::*;
use chrono::{TimeZone, Utc};

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), "x").unwrap();
}

#[test]
fn prune_logs_removes_files_older_than_30_days() {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    let now = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
    // межа: 2026-11-07 - 30 днів = 2026-10-08
    touch(d, "engine.log.2026-10-07"); // старіший за межу -> видалити
    touch(d, "engine.log.2026-01-01"); // видалити
    touch(d, "engine.log.2026-10-08"); // рівно на межі -> лишити
    touch(d, "engine.log.2026-11-07"); // свіжий
    touch(d, "engine.log"); // не за шаблоном
    touch(d, "other.log.2020-01-01"); // чужий файл
    touch(d, "engine.log.notadate");

    let n = prune_logs(d, 30, now).unwrap();
    assert_eq!(n, 2);

    let mut left: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![
            "engine.log",
            "engine.log.2026-10-08",
            "engine.log.2026-11-07",
            "engine.log.notadate",
            "other.log.2020-01-01",
        ]
    );
}

#[test]
fn prune_logs_on_missing_dir_is_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let n = prune_logs(&tmp.path().join("nope"), 30, Utc::now()).unwrap();
    assert_eq!(n, 0);
}
