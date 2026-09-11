//! engine 对账逻辑单测 (纯函数, 不触网).

use super::*;
use std::time::Duration;

fn cfg(running: bool, connections: u32, target_ops: u64) -> WorkloadConfig {
    WorkloadConfig {
        running,
        connections,
        target_ops,
        ..Default::default()
    }
}

fn snapshot(workers: u32, desired: &WorkloadConfig, epoch: u64) -> EngineSnapshot {
    EngineSnapshot {
        workers,
        spec: ConnSpec::from_config(desired),
        epoch,
        started: workers,
    }
}

#[test]
fn starts_workers_when_epoch_advances() {
    let desired = cfg(true, 8, 5000);
    assert_eq!(
        plan_actions(&snapshot(0, &desired, 0), &desired, 1),
        vec![Action::Restart { workers: 8 }]
    );
}

#[test]
fn same_epoch_does_not_scale_or_set_rate() {
    let desired = cfg(true, 2, 1000);
    assert!(plan_actions(&snapshot(8, &desired, 1), &desired, 1).is_empty());

    let scaled_up = cfg(true, 9, 1000);
    assert!(plan_actions(&snapshot(2, &scaled_up, 1), &scaled_up, 1).is_empty());

    let faster = cfg(true, 4, 20000);
    assert!(plan_actions(&snapshot(4, &faster, 1), &faster, 1).is_empty());
}

#[test]
fn start_while_running_restarts_on_new_epoch() {
    let desired = cfg(true, 16, 8000);
    assert_eq!(
        plan_actions(&snapshot(32, &desired, 1), &desired, 2),
        vec![Action::Restart { workers: 16 }]
    );
}

#[test]
fn repairs_missing_workers_to_started_count() {
    let desired = cfg(true, 8, 20000);
    let current = EngineSnapshot {
        workers: 3,
        spec: ConnSpec::from_config(&desired),
        epoch: 1,
        started: 8,
    };
    assert_eq!(
        plan_actions(&current, &desired, 1),
        vec![Action::AddWorkers(5)]
    );
}

#[test]
fn form_connection_change_same_epoch_does_not_scale() {
    let desired = cfg(true, 64, 20000);
    let current = EngineSnapshot {
        workers: 32,
        spec: ConnSpec::from_config(&desired),
        epoch: 1,
        started: 32,
    };
    assert!(plan_actions(&current, &desired, 1).is_empty());
}

#[test]
fn stops_all_workers_when_not_running() {
    let desired = cfg(false, 8, 20000);
    assert_eq!(
        plan_actions(&snapshot(8, &desired, 1), &desired, 1),
        vec![Action::RemoveWorkers(8)]
    );
    assert!(plan_actions(&snapshot(0, &desired, 1), &desired, 1).is_empty());
}

#[test]
fn endpoint_change_without_new_epoch_does_not_restart() {
    let base = cfg(true, 4, 20000);
    let moved = WorkloadConfig {
        endpoints: vec!["127.0.0.1:6380".to_string()],
        ..base.clone()
    };
    assert!(plan_actions(&snapshot(4, &base, 1), &moved, 1).is_empty());
}

#[test]
fn start_after_stop_uses_new_epoch() {
    let desired = cfg(true, 4, 20000);
    let actions = plan_actions(&snapshot(0, &desired, 0), &desired, 1);
    assert_eq!(actions, vec![Action::Restart { workers: 4 }]);
}

#[test]
fn doubles_connect_backoff_until_cap() {
    assert_eq!(
        next_connect_backoff(Duration::from_millis(500)),
        Duration::from_secs(1)
    );
    assert_eq!(
        next_connect_backoff(Duration::from_secs(8)),
        Duration::from_secs(10)
    );
    assert_eq!(
        next_connect_backoff(Duration::from_secs(10)),
        Duration::from_secs(10)
    );
}
