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
        endpoint: "127.0.0.1:6380".to_string(),
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

#[test]
fn runtime_state_watermarks_reset_on_epoch_bump() {
    let state = RuntimeState::default();
    state.watermarks.string.main.store(100, Ordering::Relaxed);
    state.watermarks.string.ttl.store(50, Ordering::Relaxed);
    state.watermarks.string.churn.store(25, Ordering::Relaxed);

    state.bump_run_epoch();

    assert_eq!(state.watermarks.string.main.load(Ordering::Relaxed), 0);
    assert_eq!(state.watermarks.string.ttl.load(Ordering::Relaxed), 0);
    assert_eq!(state.watermarks.string.churn.load(Ordering::Relaxed), 0);
}

/// 派发从暂停恢复后应清掉暂停原因, 避免控制台一直 toast 过期的「已暂停派发」.
#[tokio::test]
async fn resume_clears_stale_pause_error() {
    let state = RuntimeState::default();
    state
        .set_paused(true, Some("集群状态为 unreachable, 已暂停派发".into()))
        .await;
    assert!(state.is_paused());
    assert!(state.last_error().await.is_some());

    state.set_paused(false, None).await;
    assert!(!state.is_paused());
    assert!(
        state.last_error().await.is_none(),
        "恢复派发后 last_error 应清空"
    );

    state
        .set_paused(true, Some("全部节点不可达, 已暂停派发".into()))
        .await;
    state.resume_dispatch().await;
    assert!(!state.is_paused());
    assert!(
        state.last_error().await.is_none(),
        "点启动 resume_dispatch 也应清掉过期暂停错误"
    );
}

#[tokio::test]
async fn user_pause_survives_probe_clearing_dispatch_pause() {
    let state = RuntimeState::default();
    state.set_user_paused(true);
    assert!(state.is_paused());
    state.set_paused(false, None).await;
    assert!(state.is_paused(), "探活恢复不得清掉用户点的暂停");
    state.resume_dispatch().await;
    assert!(!state.is_paused());
}
