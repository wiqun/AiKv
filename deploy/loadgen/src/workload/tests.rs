//! workload 模块单测.

use std::collections::HashMap;
use std::sync::atomic::Ordering;

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
    let wm = Watermarks::default();
    assert_eq!(plan_batch(&cfg, &wm, &mut rng).len(), 8);
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
    let wm = Watermarks::default();
    for _ in 0..500 {
        for planned in plan_batch(&cfg, &wm, &mut rng) {
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
    let cfg = WorkloadConfig {
        keyspace: 10,
        pipeline: 50,
        miss_ratio: 0.0,
        mix: Mix {
            set: 1,
            get: 1,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(11);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
    assert!(plan.iter().all(|p| p.key.contains("loadgen:key:")));

    let tagged = WorkloadConfig {
        use_hashtag: true,
        ..cfg
    };
    let plan = plan_batch(&tagged, &wm, &mut rng);
    assert!(plan.iter().all(|p| p.key.starts_with("{loadgen}:key:")));

    let target_slot_cfg = WorkloadConfig {
        target_slot: Some(1234),
        ..tagged
    };
    let plan = plan_batch(&target_slot_cfg, &wm, &mut rng);
    assert!(plan.iter().all(|p| key_slot(&p.key) == 1234));
}

#[test]
fn tag_for_slot_matches_requested_slot() {
    for slot in [0, 1, 100, 1024, 7490, 10438, 16383] {
        let tag = tag_for_slot(slot);
        assert_eq!(crc16(tag.as_bytes()) % 16384, slot);
    }
}

#[test]
fn miss_keys_use_miss_segment() {
    let cfg = WorkloadConfig {
        keyspace: 10,
        pipeline: 200,
        miss_ratio: 1.0,
        mix: Mix {
            set: 50,
            get: 50,
            del: 0,
            mget: 50,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(3);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);

    // SET 写命令必须走正常数据区，绝不能污染 miss 区
    assert!(plan
        .iter()
        .filter(|p| p.op == Op::Set)
        .all(|p| p.key.contains(":key:")));

    // 读命令在 miss_ratio: 1.0 时全部使用 :miss: 隔离段
    assert!(plan
        .iter()
        .filter(|p| p.op == Op::Get)
        .all(|p| p.key.contains(":miss:")));
    assert!(plan.iter().filter(|p| p.op == Op::Mget).all(|p| {
        p.key.contains(":miss:") && p.second_key.as_ref().is_some_and(|k| k.contains(":miss:"))
    }));
}

fn all_keys(plan: &[PlannedOp]) -> Vec<&str> {
    let mut keys = Vec::new();
    for planned in plan {
        keys.push(planned.key.as_str());
        if let Some(second) = &planned.second_key {
            keys.push(second.as_str());
        }
    }
    keys
}

#[test]
fn cluster_batch_shares_one_hash_tag() {
    let cfg = WorkloadConfig {
        pipeline: 32,
        keyspace: 10_000,
        miss_ratio: 0.5,
        mix: Mix {
            set: 20,
            get: 20,
            del: 10,
            mget: 40,
            incr: 5,
            expire: 5,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(99);
    let wm = Watermarks::default();
    wm.main.store(10, Ordering::Relaxed);
    for _ in 0..50 {
        let plan = plan_batch(&cfg, &wm, &mut rng);
        let keys = all_keys(&plan);
        let tags: std::collections::HashSet<_> = keys.iter().map(|k| hash_tag(k)).collect();
        assert_eq!(tags.len(), 1, "cluster 一轮 pipeline 必须同 slot: {keys:?}");
        assert!(
            tags.iter().all(|t| t.is_some()),
            "cluster key 必须带 hash tag"
        );
    }
}

#[test]
fn single_mode_keys_have_no_hash_tag() {
    let cfg = WorkloadConfig {
        mode: crate::config::TargetMode::Single,
        pipeline: 20,
        miss_ratio: 0.0,
        mix: Mix {
            set: 1,
            get: 0,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(4);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
    assert!(plan.iter().all(|p| p.key.starts_with("loadgen:key:")));
    assert!(all_keys(&plan).iter().all(|k| hash_tag(k).is_none()));
}

#[test]
fn incr_uses_int_segment() {
    let cfg = WorkloadConfig {
        pipeline: 40,
        miss_ratio: 1.0,
        mix: Mix {
            set: 0,
            get: 0,
            del: 0,
            mget: 0,
            incr: 1,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(21);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
    assert!(plan.iter().all(|p| p.key.contains(":int:")));
}

#[test]
fn readonly_mode_never_plans_writes() {
    let cfg = WorkloadConfig {
        readonly: true,
        pipeline: 200,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(5);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
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
    let wm = Watermarks::default();
    for planned in plan_batch(&cfg, &wm, &mut rng) {
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
        ttl_seconds: 120,
        pipeline: 100,
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(13);
    let wm = Watermarks::default();
    let plan = plan_batch(&with_ttl, &wm, &mut rng);
    assert!(plan
        .iter()
        .all(|p| p.op != Op::Set || p.ttl_seconds == Some(120)));

    let without_ttl = WorkloadConfig {
        ttl_ratio: 0.0,
        ..with_ttl
    };
    let plan = plan_batch(&without_ttl, &wm, &mut rng);
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
            value_size: 16,
            set_seq: Some(42),
            ttl_seconds: Some(TTL_SECONDS),
        },
        PlannedOp {
            op: Op::Mget,
            key: "k".to_string(),
            second_key: Some("k2".to_string()),
            value_size: 0,
            set_seq: None,
            ttl_seconds: None,
        },
    ];
    let value = [b'x'; 32];
    let pipe = build_pipeline(&plan, 100, &value);
    assert_eq!(pipe.cmd_iter().count(), 2);
}

#[test]
fn value_payload_contains_round_and_seq() {
    let pad = [b'y'; 64];
    // 第 0 轮 (seq 5, keyspace 100) -> v0:5:...
    let payload = build_value_payload(5, 100, 32, &pad);
    assert_eq!(payload.len(), 32);
    let s = std::str::from_utf8(&payload).unwrap();
    assert!(s.starts_with("v0:5:"));
    assert!(s.ends_with("yyyyyyyy"));

    // 第 2 轮 (seq 250, keyspace 100) -> v2:250:...
    let payload2 = build_value_payload(250, 100, 20, &pad);
    assert_eq!(payload2.len(), 20);
    let s2 = std::str::from_utf8(&payload2).unwrap();
    assert!(s2.starts_with("v2:250:"));
}

#[test]
fn watermark_advances_only_with_set() {
    let cfg = WorkloadConfig {
        pipeline: 100,
        keyspace: 10_000,
        mix: Mix {
            set: 20,
            get: 40,
            del: 10,
            mget: 10,
            incr: 10,
            expire: 10,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(88);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
    let set_count = plan.iter().filter(|p| p.op == Op::Set).count() as u64;
    let main_sets = wm.main.load(Ordering::Relaxed);
    let ttl_sets = wm.ttl.load(Ordering::Relaxed);
    assert_eq!(main_sets + ttl_sets, set_count);
}

#[test]
fn hit_reads_stay_within_watermark_pool() {
    let cfg = WorkloadConfig {
        pipeline: 500,
        keyspace: 10_000,
        miss_ratio: 0.0, // 100% 命中
        mix: Mix {
            set: 0,
            get: 100,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(42);
    let wm = Watermarks::default();
    wm.main.store(50, Ordering::Relaxed);
    let plan = plan_batch(&cfg, &wm, &mut rng);
    for p in &plan {
        assert_eq!(p.op, Op::Get);
        assert!(p.key.contains("loadgen:key:"));
        let parts: Vec<&str> = p.key.split(':').collect();
        let idx: u64 = parts.last().unwrap().parse().unwrap();
        assert!(idx < 50, "index {idx} should be < watermark (50)");
    }
}

#[test]
fn zero_watermark_safe_fallback() {
    let cfg = WorkloadConfig {
        pipeline: 100,
        keyspace: 10_000,
        miss_ratio: 0.0,
        mix: Mix {
            set: 0,
            get: 100,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(17);
    let wm = Watermarks::default();
    let plan = plan_batch(&cfg, &wm, &mut rng);
    for p in &plan {
        assert!(p.key.ends_with(":loadgen:key:0"));
    }
}

#[test]
fn read_hit_miss_distribution_matches_ratio() {
    let cfg = WorkloadConfig {
        pipeline: 1000,
        keyspace: 10_000,
        miss_ratio: 0.2, // 20% 未命中, 80% 命中
        mix: Mix {
            set: 0,
            get: 100,
            del: 0,
            mget: 0,
            incr: 0,
            expire: 0,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(777);
    let wm = Watermarks::default();
    wm.main.store(100, Ordering::Relaxed);
    let mut hit_count = 0;
    let mut miss_count = 0;
    for _ in 0..10 {
        let plan = plan_batch(&cfg, &wm, &mut rng);
        for p in &plan {
            if p.key.contains(":key:") {
                hit_count += 1;
            } else if p.key.contains(":miss:") {
                miss_count += 1;
            }
        }
    }
    let total = hit_count + miss_count;
    let actual_hit_ratio = hit_count as f64 / total as f64;
    assert!(
        (actual_hit_ratio - 0.8).abs() < 0.03,
        "expected ~0.8, got {actual_hit_ratio}"
    );
}

#[test]
fn test_make_key_segments() {
    let cfg = WorkloadConfig {
        key_prefix: "test".into(),
        ..Default::default()
    };
    assert_eq!(make_key(&cfg, 42, KeySegment::Main, None), "test:key:42");
    assert_eq!(make_key(&cfg, 42, KeySegment::Miss, None), "test:miss:42");
    assert_eq!(make_key(&cfg, 42, KeySegment::Ttl, None), "test:ttl:42");
    assert_eq!(make_key(&cfg, 42, KeySegment::Churn, None), "test:churn:42");
    assert_eq!(make_key(&cfg, 42, KeySegment::Int, None), "test:int:42");
}

#[test]
fn test_intent_segregated_hit_ratio() {
    // 验证方案 A: 在高比例 DEL (20%) 与高比例 TTL (30%) 的混合场景下
    // DEL 严格作用于 :churn:，带 TTL 的 SET 严格作用于 :ttl:
    // 普通 SET 和 GET 严格在 :key: 闭环，命中率不被 DEL/TTL 腐蚀
    let cfg = WorkloadConfig {
        pipeline: 500,
        keyspace: 10_000,
        miss_ratio: 0.1, // 90% 命中
        ttl_ratio: 0.5,
        ttl_seconds: 60,
        mix: Mix {
            set: 20,
            get: 40,
            del: 20,
            mget: 10,
            incr: 5,
            expire: 5,
        },
        ..Default::default()
    };
    let mut rng = StdRng::seed_from_u64(999);
    let wm = Watermarks::default();
    wm.main.store(200, Ordering::Relaxed);
    wm.ttl.store(100, Ordering::Relaxed);

    let mut hit_count = 0;
    let mut miss_count = 0;
    let mut del_count = 0;

    for _ in 0..20 {
        let plan = plan_batch(&cfg, &wm, &mut rng);
        for p in &plan {
            match p.op {
                Op::Get => {
                    if p.key.contains(":key:") {
                        hit_count += 1;
                    } else if p.key.contains(":miss:") {
                        miss_count += 1;
                    }
                }
                Op::Del => {
                    del_count += 1;
                    // DEL 绝不能作用在主数据段 :key:，必须在 :churn:
                    assert!(p.key.contains(":churn:"), "DEL 必须在 churn 段: {}", p.key);
                }
                Op::Expire => {
                    assert!(p.key.contains(":ttl:"), "EXPIRE 必须在 ttl 段: {}", p.key);
                }
                Op::Incr => {
                    assert!(p.key.contains(":int:"), "INCR 必须在 int 段: {}", p.key);
                }
                Op::Set => {
                    if p.ttl_seconds.is_some() {
                        assert!(
                            p.key.contains(":ttl:"),
                            "带 TTL 的 SET 必须在 ttl 段: {}",
                            p.key
                        );
                    } else {
                        assert!(p.key.contains(":key:"), "普通 SET 必须在 key 段: {}", p.key);
                    }
                }
                _ => {}
            }
        }
    }

    assert!(del_count > 0, "必须生成了 DEL 命令");
    let total_get = hit_count + miss_count;
    let actual_hit_ratio = hit_count as f64 / total_get as f64;
    assert!(
        (actual_hit_ratio - 0.9).abs() < 0.03,
        "在存在 DEL 和 TTL 时，读命中率仍应稳定在 ~0.9，实际为 {actual_hit_ratio}"
    );
}
