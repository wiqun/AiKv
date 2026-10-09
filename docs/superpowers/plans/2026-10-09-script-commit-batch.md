# 脚本提交一次写入 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 一条 EVAL 成功结束时，把脚本里改过的全部 key 一次提交，不再每个 key 各等一次 Raft。

**Architecture:** `ScriptTransaction::commit` 仍先在本地把 TTL 和类型化的值解析完，再调用 `KvStorage::apply_stored_batch`。`KvStorageAdapter` 把这些值连同 WATCH 元数据收成一批，只调用一次 `StorageAdapter::apply_writes`。集群侧已有的凑批器继续按最多 512 笔拆提案。内存引擎走 trait 默认实现，逐条 `set_typed` / `delete`，行为与现在一致。

**Tech Stack:** Rust, `KvStorage` / `KvStorageAdapter` / `StorageAdapter::apply_writes`, `cargo test --test storage`, `cargo test --test commands`.

## Global Constraints

- 不改 WAL / LSM / Raft 日志格式、磁盘格式、Redis 线协议。
- `MULTI/EXEC` 仍然不是一条 Raft 日志。这次只改脚本 `commit`。
- `sync_wal` 默认保持关闭。
- 不推送，不 `generate_configs`，不清空远端数据卷。
- 不提交 `deploy/docker-compose.cluster.yaml` 的 cpuset 改动。
- 本计划在当前分支 `feat/observability-tuning`、当前会话执行。提交留到用户明确要求时再做。
- 复用已有的 `apply_writes`、`next_watch_meta_put`、`ExpiresCounters`。不新写提案路径。

---

### Task 1: 脚本 commit 一次 apply_writes

**Files:**
- Modify: `src/storage/types.rs` (`KvStorage::apply_stored_batch` 默认实现)
- Modify: `src/storage/adapter.rs` (`KvStorageAdapter` 覆盖)
- Modify: `src/command/script/transaction.rs` (`commit`)
- Modify: `docs/modules/05-commands-extended.md`（一句：成功时一次 `apply_writes`）

**Interfaces:**
- Consumes: `StorageAdapter::apply_writes`，`KvStorageAdapter::serialize` / `encode` / `get_raw` / `next_watch_meta_put`，`ExpiresCounters::on_ttl_write` / `on_ttl_clear`
- Produces: `KvStorage::apply_stored_batch(&self, db: usize, ops: Vec<(Vec<u8>, Option<StoredValue>)>) -> Result<()>`。`None` 表示删除。空批次不调用 `apply_writes`。

- [x] **Step 1: trait 默认实现**

在 `KvStorage` 上增加 `apply_stored_batch`。默认逐条调用已有的 `delete` 或 `set_typed`，让 `MemoryEngine` 不用改。

- [x] **Step 2: 适配器一次写入**

`KvStorageAdapter` 覆盖该方法：

- 删除：key 不存在则跳过，不写 WATCH 元数据。存在则 `Delete` 编码后的 key；旧值带 TTL 时记一次 `on_ttl_clear`。
- 写入：`serialize` 后 `Put`。`expires_at` 为 `Some` 时记一次 `on_ttl_write`。
- 实际写入的每个用户 key，若当前有 WATCH，把 `next_watch_meta_put` 的结果放进同一批。
- 批次非空时只调用一次 `self.storage.apply_writes`。计数在写入成功之后更新。

- [x] **Step 3: commit 改为收集后一次提交**

`ScriptTransaction::commit` 先对每个 key 做现在这些本地读（保留 TTL、只改过期时间），得到 `Vec<(Vec<u8>, Option<StoredValue>)>`，然后一次 `apply_stored_batch`。需要 `release_seen_ttl` 的次数在写入成功后照旧扣减。这些扣减是进程内计数，不进 Raft。

### Task 2: 验证一次提交，并保住现有脚本行为

**Files:**
- Create: `tests/modules/storage/script_commit_batch.rs`
- Modify: `tests/modules/storage/mod.rs`

- [x] **Step 1: 计数适配器**

测试用的 `StorageAdapter` 单独计数 `apply_writes`、`put_many`、`set`。`apply_writes` 自己写入哈希表，不转调 `set`。

- [x] **Step 2: EVAL 多 key**

用 `KvStorageAdapter` + `CommandRouter` 执行一条脚本：`SET`、`HSET`、`ZADD`，并 `DEL` 一个事先存在的 key。断言：

- `apply_writes` 为 1
- `put_many` 与 `set` 在这次 EVAL 期间为 0
- 三个写 key 能读回，被删的 key 不存在

另测一条只有 `return 1` 的脚本，以及只 `DEL` 不存在的 key：`apply_writes` 仍为 0。

- [x] **Step 3: 跑测试**

```bash
cargo test --test storage script_commit_batch -- --test-threads=8
cargo test --test commands -- script:: -- --test-threads=8
```

期望：新测试通过；现有脚本测试通过（内存引擎仍走逐条默认实现）。
