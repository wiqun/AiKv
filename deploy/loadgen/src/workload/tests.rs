//! workload 模块单测.

use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::SeedableRng;

use super::*;
use crate::config::WorkloadConfig;

#[test]
fn mix_sampling_matches_weights() {
    let mix = Mix {
        set: 40,
        get: 40,
        del: 5,
        mget: 10,
        incr: 3,
        expire: 2,
    };
    let total = mix.total();
    let mut rng = StdRng::seed_from_u64(42);
    let mut counts: HashMap<Op, u32> = HashMap::new();
    let samples = 200_000u32;
    for _ in 0..samples {
        let op = pick_op(&mix, rng.gen_range(0..total));
        *counts.entry(op).or_insert(0) += 1;
    }
    for op in Op::ALL {
        let expected = f64::from(op.weight(&mix)) / f64::from(total);
        let actual = f64::from(counts.get(&op).copied().unwrap_or(0)) / f64::from(samples);
        assert!(
            (expected - actual).abs() < 0.02,
            "{op:?}: expected={expected:.3} actual={actual:.3}"
        );
    }
}

#[test]
fn batch_length_matches_pipeline() {
    let cfg = WorkloadConfig {
        pipeline: 8,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(1);
    assert_eq!(plan_batch(&cfg, &mut rng).len(), 8);
}

#[test]
fn op_distribution_matches_weights() {
    let cfg = WorkloadConfig {
        pipeline: 100,
        keyspace: 1_000,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(7);
    let mut counts: HashMap<Op, u32> = HashMap::new();
    let mut total = 0u32;
    for _ in 0..500 {
        for planned in plan_batch(&cfg, &mut rng) {
            *counts.entry(planned.op).or_insert(0) += 1;
            total += 1;
        }
    }
    for op in Op::ALL {
        let expected = f64::from(op.weight(&cfg.mix)) / f64::from(cfg.mix.total());
        let actual = f64::from(counts.get(&op).copied().unwrap_or(0)) / f64::from(total);
        assert!(
            (expected - actual).abs() < 0.02,
            "{op:?}: expected={expected:.3} actual={actual:.3}"
        );
    }
}

#[test]
fn keys_follow_prefix_and_hashtag() {
    // miss_ratio 归零, 保证 key 都是常规段 (miss 段由专门用例覆盖)
    let cfg = WorkloadConfig {
        keyspace: 10,
        pipeline: 50,
        miss_ratio: 0.0,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(11);
    let plan = plan_batch(&cfg, &mut rng);
    assert!(plan.iter().all(|p| p.key.starts_with("loadgen:key:")));

    let tagged = WorkloadConfig {
        use_hashtag: true,
        ..cfg
    };
    let plan = plan_batch(&tagged, &mut rng);
    assert!(plan.iter().all(|p| p.key.starts_with("{loadgen}:key:")));
}

#[test]
fn miss_keys_use_miss_segment() {
    let cfg = WorkloadConfig {
        keyspace: 10,
        pipeline: 200,
        miss_ratio: 1.0,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(3);
    let plan = plan_batch(&cfg, &mut rng);
    assert!(plan.iter().all(|p| p.key.contains(":miss:")));
}

#[test]
fn readonly_mode_never_plans_writes() {
    let cfg = WorkloadConfig {
        readonly: true,
        pipeline: 200,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(5);
    let plan = plan_batch(&cfg, &mut rng);
    assert!(plan.iter().all(|p| matches!(p.op, Op::Get | Op::Mget)));
}

#[test]
fn value_size_stays_within_range() {
    let cfg = WorkloadConfig {
        value_size_min: 16,
        value_size_max: 64,
        pipeline: 200,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(9);
    for planned in plan_batch(&cfg, &mut rng) {
        if planned.op == Op::Set {
            assert!(
                (16..=64).contains(&planned.value_size),
                "size={}",
                planned.value_size
            );
        }
    }
}

#[test]
fn ttl_ratio_controls_set_ttl() {
    let with_ttl = WorkloadConfig {
        ttl_ratio: 1.0,
        pipeline: 100,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(13);
    let plan = plan_batch(&with_ttl, &mut rng);
    assert!(plan
        .iter()
        .all(|p| p.op != Op::Set || p.ttl_seconds == Some(TTL_SECONDS)));

    let without_ttl = WorkloadConfig {
        ttl_ratio: 0.0,
        ..with_ttl
    };
    let plan = plan_batch(&without_ttl, &mut rng);
    assert!(plan
        .iter()
        .all(|p| p.op != Op::Set || p.ttl_seconds.is_none()));
}

#[test]
fn pipeline_maps_planned_ops() {
    let plan = vec![
        PlannedOp {
            op: Op::Set,
            key: "k".to_string(),
            second_key: None,
            value_size: 3,
            ttl_seconds: Some(TTL_SECONDS),
        },
        PlannedOp {
            op: Op::Mget,
            key: "k".to_string(),
            second_key: Some("k2".to_string()),
            value_size: 0,
            ttl_seconds: None,
        },
    ];
    let value = [b'x'; 8];
    let pipe = build_pipeline(&plan, &value);
    assert_eq!(pipe.cmd_iter().count(), 2);
}
