# Lua EVAL 兼容修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 TimeSeq / Hauler 这种「只声明带哈希标签的前缀、在脚本里拼出真实 key」的 EVAL 能在 aikv 上执行，并补齐这些脚本实际调用的命令。

**Architecture:** EVAL/EVALSHA 不再要求 `redis.call` 的 key 与 KEYS 逐字节相同。集群构建下，脚本访问的 key 必须与第一把声明 key 落在同一 slot；没有声明 key 时，以脚本里第一次访问的 key 的 slot 为锚。`SET` 选项复用 `parse_set_options`，写入仍进 `ScriptTransaction`。`ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` / `ZREVRANGE` 复用 zset `helpers` 的边界与排序。`TTL` 先看事务里的过期记录，再回落到存储。

**Tech Stack:** Rust, mlua, aikv `CommandRouter` 内存引擎单测, `cargo test --test commands`, 集群特性下的同 slot 测试。

## Global Constraints

- 不改 WAL / LSM / Raft、磁盘格式、Redis 线协议。
- 不推送，不 `generate_configs`，不清空远端数据卷。
- 不提交 `deploy/docker-compose.cluster.yaml` 的 cpuset 改动。
- 集群 slot 使用已有的 `aidb::cluster::router::key_to_slot`（含 hash tag）。
- 脚本写操作仍只进 `ScriptTransaction`，成功后一次 `commit`。
- 本计划在当前分支 `feat/observability-tuning`、当前会话执行。提交留到用户明确要求时再做。

---

### Task 1: EVAL 按 Redis 允许脚本内拼 key

**Files:**
- Modify: `src/command/script.rs` (`execute_script`, `install_redis_api`)
- Modify: `src/command/script/execute/mod.rs` (`validate_keys`)
- Modify: `tests/modules/command/script.rs`
- Modify: `docs/modules/05-commands-extended.md`（把「KEYS 逐字节校验」改成同 slot 规则）

**Interfaces:**
- Consumes: `aidb::cluster::router::key_to_slot`（仅 `feature = "cluster"`）
- Produces: `ScriptAccess`，由 `install_redis_api` 传给 `redis_call_async`。集群下 `anchor: Option<u16>` 来自 `KEYS[1]`，`seen: Rc<Cell<Option<u16>>>` 在 call/pcall 之间共享。

- [ ] **Step 1: 改失败测试**

`test_script_keys_validation` 改为：`KEYS[1]` 是 `{tag}:`，脚本 `SET/GET` `KEYS[1].."id"`，返回 `v`。

`test_script_json_mget_keys_validation` 改为同一 hash tag 的第二把 key，缺 key 得到 null，而不是 undeclared。

`test_script_json_keys_validation` 改为 `JSON.GET` 不存在的 key 返回 null。

新增 `#[cfg(feature = "cluster")]` 测试：`KEYS[1]={slot-a}:p`，`redis.call('GET', ARGV[1])` 且 `ARGV[1]={slot-b}:p`，错误信息包含 `same slot`。测试里先断言两把 key 的 slot 不同。

- [ ] **Step 2: 实现**

删除「key 必须属于声明集合」的判断。`command_key_indices` 保留，用来找出命令里的 key 参数。

集群下每个 key 参数的 slot 必须等于锚点。锚点优先是第一把声明 key；没有声明 key 时，第一把被访问的 key 写入 `seen`。不一致时返回：

`ERR Script attempted to access keys that do not hash to the same slot`

非集群构建不做这道检查。

- [ ] **Step 3: 验证**

```bash
cargo test --test commands --lib -- script:: -- --test-threads=8
```

期望：脚本测试通过，不再出现 undeclared key 断言。

```bash
cargo test --features cluster --test commands script::test_script_cross_slot -- --test-threads=8
```

期望：跨 slot 测试通过。同一次编译里原有的「拼 key 成功」测试也要通过。

### Task 2: 脚本内 SET 选项、TTL、HSETNX、ZSet 范围

**Files:**
- Modify: `src/command/string.rs`：`SetCondition`、`SetOptions`、`parse_set_options` 改为 `pub(crate)`
- Modify: `src/command/script/execute/string.rs`：`exec_set` 变异步并吃选项
- Modify: `src/command/script/transaction.rs`：增加 `ttl_ms`
- Modify: `src/command/script/execute/mod.rs`：分发 `TTL`，`SET` 改为异步
- Modify: `src/command/script/execute/hash.rs`：`exec_hsetnx`
- Modify: `src/command/zset/mod.rs`：`pub(crate) mod helpers`
- Modify: `src/command/zset/helpers.rs`：范围辅助改为 `pub(crate)`
- Modify: `src/command/script/execute/zset.rs`：`ZREVRANGE` / `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE`
- Test: `tests/modules/command/script.rs`

**Interfaces:**
- Consumes: `parse_set_options`；`helpers::{apply_limit, members_to_resp, parse_limit_args, parse_score_bound, score_in_range, sorted_by_score, bytes_to_str}`
- Produces:
  - `exec_set(storage, txn, args) -> Result<RespValue>`
  - `ScriptTransaction::ttl_ms(&self, storage, key) -> Result<Option<i64>>`
  - `exec_hsetnx`、`exec_zrevrange`、`exec_zrangebyscore`、`exec_zrevrangebyscore`

行为：

- `SET key value EX|PX|EXAT|PXAT` 写入字符串，并把过期时间放进 `expire_at`。无过期选项且非 `KEEPTTL` 时 `set_expire_at(None)`，清掉旧 TTL。
- `NX` 在 key 已存在时不写，返回 null。`XX` 在 key 不存在时不写，返回 null。`XX EX` 两者都生效。
- `GET` 成功时返回旧值；条件不满足时：`NX` 且已存在返回旧值，`XX` 且不存在返回 null。
- `TTL`：缓冲里已删除 → `-2`；`expire_at` 有值 → 剩余秒；显式 `None` → `-1`；缓冲里新建且存储中无此 key → `-1`；否则用 `storage.ttl`。秒的换算与 `map_ttl_seconds` 相同（不足 1 秒为 0，无过期 `-1`，不存在 `-2`）。
- `HSETNX key field value`：字段已存在返回 `0` 且不改值；否则写入并返回 `1`。
- `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` 支持 `WITHSCORES` 与 `LIMIT offset count`，边界规则与命令路径相同（倒序时 min/max 对调）。
- `ZREVRANGE` 在现有按下标范围的结果上反转排序，保留 `WITHSCORES`。

- [ ] **Step 1: 测试**

同一 `script.rs` 增加四条：

1. `SET` + `EX 60` 后 `TTL` 落在 `1..=60`。
2. 不存在的 key `SET ... XX` 返回 `RespValue::Null`；先 `SET` 再 `SET ... XX EX 30`，`GET` 为新值且 `TTL` 落在 `1..=30`。
3. `HSETNX` 第一次 `1`、第二次 `0`，`HGET` 仍是第一次的值。
4. `ZADD` 三个成员后，`ZRANGEBYSCORE ... LIMIT 0 1`、`ZREVRANGE 0 0`、`ZREVRANGEBYSCORE ... WITHSCORES` 拼成 `a:c:c:3`。

- [ ] **Step 2: 实现上述行为**

- [ ] **Step 3: 验证**

```bash
cargo test --test commands script:: -- --test-threads=8
```

期望全部通过。然后：

```bash
cargo fmt --check
RUSTFLAGS='-D warnings' cargo clippy --all-targets --all-features -- -D warnings
```

clippy 若因既有告警无法作为本变更门禁，改跑只覆盖本次目标的测试，并在结果里写明未跑全量 clippy 的原因。

### Task 3: 远端回归

**Files:** 不改测试仓库。部署物是本分支编出的 `aikv:dev`。

- [ ] 本地单测通过后，在 aikv 目录执行 `./deploy/aikv-cluster.sh build --local`。
- [ ] 把镜像载到 192.168.1.112 与 192.168.1.113，`docker compose -p aikv-cluster -f docker-compose.cluster.yaml up -d --force-recreate --no-deps` 重建 aikv-1 到 aikv-6。不生成配置，不清卷。
- [ ] `timeout 180 dotnet test` 跑 `/root/code/wiqun/tests/tl.timeseq` 的 `TL.TimeSeq.sln`。
- [ ] `timeout 240 dotnet test` 跑 `/root/code/wiqun/tests/tl.hauler` 的 `TL.Hauler.sln`，过滤 `FullyQualifiedName!~Benchmark`。
- [ ] 记录通过数、失败命令和是否超时。基准测试仍不跑，它们会在收不齐消息时一直 `Join`。

---

## 自检

- 未声明 key 可访问：Task 1。
- 集群跨 slot 拒绝：Task 1 的 cluster 测试。
- `SET EX/XX`、`TTL`、`HSETNX`、`ZRANGEBYSCORE`、`ZREVRANGEBYSCORE`、`ZREVRANGE`：Task 2。TimeSeq 与 Hauler 脚本里出现的就是这些。
- 原失败套件：Task 3。
