//! engine 对账逻辑单测 (纯函数, 不触网).

use super::*;

fn cfg(running: bool, connections: u32, target_ops: u64) -> WorkloadConfig {
    WorkloadConfig {
        running,
        connections,
        target_ops,
        ..Default::default()
    }
}

fn snapshot(workers: u32, rate: u64, desired: &WorkloadConfig) -> EngineSnapshot {
    EngineSnapshot {
        workers,
        rate,
        spec: ConnSpec::from_config(desired),
    }
}

#[test]
fn starts_workers_when_running() {
    let desired = cfg(true, 8, 5000);
    let actions = plan_actions(&snapshot(0, 0, &desired), &desired);
    assert!(actions.contains(&Action::AddWorkers(8)));
    assert!(actions.contains(&Action::SetRate(5000)));
}

#[test]
fn scales_down_and_up() {
    let desired = cfg(true, 2, 1000);
    assert_eq!(
        plan_actions(&snapshot(8, 1000, &desired), &desired),
        vec![Action::RemoveWorkers(6)]
    );

    let scaled_up = cfg(true, 9, 1000);
    assert_eq!(
        plan_actions(&snapshot(2, 1000, &scaled_up), &scaled_up),
        vec![Action::AddWorkers(7)]
    );
}

#[test]
fn rate_only_changes_when_target_differs() {
    let desired = cfg(true, 4, 20000);
    assert!(plan_actions(&snapshot(4, 20000, &desired), &desired).is_empty());
    let actions = plan_actions(&snapshot(4, 1000, &desired), &desired);
    assert_eq!(actions, vec![Action::SetRate(20000)]);
}

#[test]
fn stops_all_workers_when_not_running() {
    let desired = cfg(false, 8, 20000);
    assert_eq!(
        plan_actions(&snapshot(8, 20000, &desired), &desired),
        vec![Action::RemoveWorkers(8)]
    );
    assert!(plan_actions(&snapshot(0, 0, &desired), &desired).is_empty());
}

#[test]
fn restarts_on_endpoint_change() {
    let base = cfg(true, 4, 20000);
    let moved = WorkloadConfig {
        endpoints: vec!["127.0.0.1:6380".to_string()],
        ..base.clone()
    };
    assert_eq!(
        plan_actions(&snapshot(4, 20000, &base), &moved),
        vec![Action::Restart { workers: 4 }]
    );
}

#[test]
fn restarts_on_mode_change() {
    let base = cfg(true, 4, 20000);
    let single = WorkloadConfig {
        mode: TargetMode::Single,
        ..base.clone()
    };
    assert_eq!(
        plan_actions(&snapshot(4, 20000, &base), &single),
        vec![Action::Restart { workers: 4 }]
    );
}

#[test]
fn start_after_stop_does_not_restart() {
    let desired = cfg(true, 4, 20000);
    let actions = plan_actions(&snapshot(0, 20000, &desired), &desired);
    assert_eq!(actions, vec![Action::AddWorkers(4)]);
}
