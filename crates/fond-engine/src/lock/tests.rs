use super::*;

#[test]
fn second_lock_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("engine.lock");
    let _a = InstanceLock::acquire(&p).unwrap();
    assert!(matches!(
        InstanceLock::acquire(&p),
        Err(LockError::AlreadyRunning)
    ));
}

#[test]
fn lock_is_released_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("engine.lock");
    let a = InstanceLock::acquire(&p).unwrap();
    drop(a);
    InstanceLock::acquire(&p).unwrap();
}

#[test]
fn creates_missing_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a").join("b").join("engine.lock");
    let _a = InstanceLock::acquire(&p).unwrap();
    assert!(p.is_file());
}
