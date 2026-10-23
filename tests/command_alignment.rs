//! 命令名字对齐: Redis 8.8.3 全集、脚本白名单、业务实际调用.
//!
//! 必须以 `--features cluster` 运行. 分类清单按带 cluster 的注册表生成.
//! 业务命令没有豁免清单: 脚本里 Redis 允许的名字必须进白名单, 直接命令必须进注册表.

use std::collections::{BTreeMap, BTreeSet};

fn names(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.split_whitespace().next().expect("name").to_string())
        .collect()
}

fn redis_table() -> BTreeMap<String, bool> {
    let mut map = BTreeMap::new();
    for line in include_str!("fixtures/redis_8_8_3_commands.txt").lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().expect("command");
        let kind = parts.next().expect("SCRIPT or NOSCRIPT");
        let noscript = match kind {
            "NOSCRIPT" => true,
            "SCRIPT" => false,
            other => panic!("bad redis flag {other} for {name}"),
        };
        assert!(
            map.insert(name.to_string(), noscript).is_none(),
            "duplicate redis command {name}"
        );
    }
    map
}

fn assert_same(label: &str, recorded: &BTreeSet<String>, computed: &BTreeSet<String>) {
    let only_recorded: Vec<_> = recorded.difference(computed).collect();
    let only_computed: Vec<_> = computed.difference(recorded).collect();
    assert!(
        only_recorded.is_empty() && only_computed.is_empty(),
        "{label}\n只在清单文件里: {only_recorded:?}\n只在当前实现里: {only_computed:?}\n重跑 python3 aikv/tools/refresh_command_alignment.py"
    );
}

#[test]
fn command_names_are_classified() {
    assert!(
        aikv::command::lookup("CLUSTER").is_some(),
        "这道测试要带 --features cluster"
    );

    let redis = redis_table();
    let registry: BTreeMap<String, bool> = aikv::command::all_commands()
        .iter()
        .map(|info| (info.name.to_string(), info.flags.contains(&"noscript")))
        .collect();
    let allow_raw = aikv::command::script_commands();
    let mut sorted = allow_raw.to_vec();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        allow_raw,
        sorted.as_slice(),
        "SCRIPT_COMMANDS 必须按字典序且不重复, 才能和 dispatch 一起被核对"
    );
    let allow: BTreeSet<String> = allow_raw.iter().map(|name| (*name).to_string()).collect();

    for name in &allow {
        assert!(
            registry.contains_key(name),
            "脚本白名单里的 {name} 不在命令注册表"
        );
        assert!(!registry[name], "{name} 标了 noscript, 不能留在脚本白名单");
    }

    let redis_names: BTreeSet<String> = redis.keys().cloned().collect();
    let registry_names: BTreeSet<String> = registry.keys().cloned().collect();
    assert_same(
        "redis_unsupported.txt",
        &names(include_str!("fixtures/redis_unsupported.txt")),
        &redis_names.difference(&registry_names).cloned().collect(),
    );
    assert_same(
        "aikv_extensions.txt",
        &names(include_str!("fixtures/aikv_extensions.txt")),
        &registry_names.difference(&redis_names).cloned().collect(),
    );

    let mut gaps = BTreeSet::new();
    for (name, aikv_noscript) in &registry {
        if *aikv_noscript {
            assert!(!allow.contains(name));
            continue;
        }
        if redis.get(name).copied().unwrap_or(false) {
            assert!(
                !allow.contains(name),
                "{name} 在 Redis 8.8.3 是 NOSCRIPT, 脚本白名单不能收它"
            );
            continue;
        }
        if !allow.contains(name) {
            gaps.insert(name.clone());
        }
    }
    assert_same(
        "script_command_gaps.txt",
        &names(include_str!("fixtures/script_command_gaps.txt")),
        &gaps,
    );

    let script_business = names(include_str!("fixtures/business_script_commands.txt"));
    let direct = names(include_str!("fixtures/business_direct_commands.txt"));
    let noise = names(include_str!("fixtures/business_command_noise.txt"));
    let script_noise: BTreeSet<String> =
        script_business.difference(&redis_names).cloned().collect();
    assert_same("business_command_noise.txt", &noise, &script_noise);

    let mut missing_script = Vec::new();
    for name in &script_business {
        if noise.contains(name) {
            assert!(
                !allow.contains(name),
                "扫描噪声 {name} 不该出现在脚本白名单"
            );
            continue;
        }
        let noscript = redis[name];
        if noscript {
            assert!(
                !allow.contains(name),
                "{name} 在 Redis 里不能用于脚本, AiKv 也不该放进白名单"
            );
        } else if !allow.contains(name) {
            missing_script.push(name.clone());
        }
    }
    assert!(
        missing_script.is_empty(),
        "业务脚本命令还不能 redis.call: {missing_script:?}"
    );

    let missing_direct: Vec<_> = direct
        .iter()
        .filter(|name| !registry.contains_key(*name))
        .cloned()
        .collect();
    assert!(
        missing_direct.is_empty(),
        "业务直接命令还没注册: {missing_direct:?}"
    );
}
