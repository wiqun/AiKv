# 脚本执行热路径 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 降低每条 EVAL 的固定开销，并让同一组声明 key 上排队的脚本共用一次 Raft 提交，同时保持 Redis 命令语义和现有磁盘格式。

**Architecture:** 三阶段各自可独立合入。第一阶段给有序集合加一份只存在于内存的分数索引，`ValueType::ZSet` 仍是 `BTreeMap<member, score>`。第二阶段把 Lua 虚拟机放进池里复用，每次执行前清掉上一份脚本留下的全局表。第三阶段让声明 key 完全相同的脚本排成一条链：持锁期间按顺序在内存里执行，写集合并成一次 `apply_stored_batch`，客户端在这次提交完成前都不算成功。

**Tech Stack:** Rust, mlua 0.10 (`lua54`, `async`, `send`), `ScriptTransaction`, `GroupSetBatcher` (`SET_BATCH_MAX_OPS = 512`), `cargo test --test commands`, `cargo test --test storage`.

## Global Constraints

- 不改 WAL / LSM / Raft 日志格式、磁盘格式、Redis 线协议。
- `ValueType::ZSet` 的 serde 布局保持 `BTreeMap<Vec<u8>, f64>`。分数索引不进入 `StoredValue`。
- `MULTI/EXEC` 仍然不是一条 Raft 日志。组提交只发生在声明 key 完全相同的 EVAL / EVALSHA 之间。
- `sync_wal` 默认保持关闭。
- 脚本执行上限保持 5 秒，键锁等待保持 30 秒。不把这两项改成配置，也不按某条外部基准放宽。
- 不按脚本正文、key 名、调用方或测试名分叉。热路径对所有 EVAL / EVALSHA 和所有 ZRANGEBYSCORE / ZRANK 生效。
- 复用已有的 `apply_stored_batch`、`GroupSetBatcher`、`SET_BATCH_MAX_OPS`。不新增第二套凑批延迟。
- 不推送，不 `generate_configs`，不清空远端数据卷。
- 不提交 `deploy/docker-compose.cluster.yaml` 的 cpuset 改动。
- 提交留到用户明确要求时再做。
- 外部基准可以在三阶段都落地后另作观察。本计划的验收是命令结果、隔离性和失败时的可见性，不是某条用例的耗时数字。

---

### Task 1: 有序集合的内存分数索引

**Files:**
- Create: `src/command/zset/index.rs`
- Modify: `src/command/zset/mod.rs`（模块注释；`rank` / `range_by_index` / `range_by_score` / `zpop` 不再调用 `sorted_by_score`）
- Modify: `src/command/zset/helpers.rs`（`sorted_by_score` 改为走索引，或变成索引的薄包装；lex 路径仍用 member 序的 `BTreeMap`）
- Modify: `src/command/script/execute/zset.rs`（范围命令走同一索引）
- Modify: `src/command/script/transaction.rs`（脚本里的 zset 在第一次按分数访问时建索引，`ZADD` / `ZREM` / `ZINCRBY` 增量维护）
- Test: `tests/modules/command/zset.rs`
- Modify: `docs/modules/05-commands-extended.md` 不改；在 `src/command/zset/mod.rs` 顶部注释把「临时排序」改成「内存分数索引，磁盘仍是 member 映射」

**Interfaces:**
- Consumes: `BTreeMap<Vec<u8>, f64>`，`parse_score` 已拒绝非有限分数，`score_in_range`，`apply_limit`
- Produces:
  - `ScoreMember`：`Ord` 为 `score.partial_cmp`（有限分数下不会是 `None`）相等时再比 member 字节。`-0.0` 与 `+0.0` 按 `partial_cmp` 相等，不使用 `total_cmp`。
  - `ScoreIndex`：`from_members(&BTreeMap<Vec<u8>, f64>) -> Self`，`insert(member, score)`，`remove(member, score)`，`range(min, max, reverse) -> impl Iterator<Item = (&Vec<u8>, f64)>`
  - 迭代器在 `LIMIT` 要求的 offset + count 处停止，不先物化整段成员。
  - `rank(member, score, reverse) -> usize`：沿分数序走到该成员。`std::BTreeMap` 没有顺序统计，这里是线性计数，替换掉「克隆全部成员再全量排序」。

磁盘上的值仍然只写 member → score。脚本 `commit` 把 `BTreeMap` 放进 `ValueType::ZSet`，索引丢掉。从存储装入时重新 `from_members`。

命令路径一次调用建一次索引。脚本路径把 `ScoreIndex` 放在该 key 的脚本缓存旁边：`with_zset` / `update_zset` 读的是同一份；只读命令不把 key 标脏。`ZADD` 更新两份结构，不在下一次范围查询时整表重建。

- [x] **Step 1: 写失败测试**

在 `tests/modules/command/zset.rs` 增加三类断言，使用现有命令入口，不新开服务器：

- 相同分数按 member 字节序排列；`ZRANGEBYSCORE key -inf +inf LIMIT 0 2` 只返回前两个，后续成员不要求出现在结果里。
- 开区间 `(1`、`WITHSCORES`、`ZREVRANGEBYSCORE`、`ZRANK` / `ZREVRANK` 与现在的排序结果一致。`ZADD` 改分后再查，旧分数下找不到该成员。
- 构造一个 `StoredValue::new_zset`，`serde` 往返后的字节与「直接序列化 `ValueType::ZSet(map)`」一致；往返后再 `ZRANGEBYSCORE` 结果相同。

- [x] **Step 2: 跑测试确认失败或尚未覆盖新结构**

```bash
cargo test --test commands zset:: -- --test-threads=8
```

新的序列化一致性测试在索引被写进 `StoredValue` 时必须失败。若它已经通过，说明实现还没开始，继续 Step 3。

- [x] **Step 3: 实现索引并替换调用点**

`sorted_by_score` 的调用点改为 `ScoreIndex`。`ZRANGE` / `ZREVRANGE` 按排名走分数序迭代，只收集窗口内的成员。`ZRANGEBYLEX` 继续走 member 序，不建分数索引。

脚本侧在 `ExtendedBatchOp::SetZSet` 旁增加 `Option<ScoreIndex>`。第一次按分数读时用当前 `BTreeMap` 填充；`update_zset` 里的插入和删除同时改索引。`commit` 只移动 `BTreeMap`。

- [x] **Step 4: 跑测试**

```bash
cargo test --test commands zset:: -- --test-threads=8
cargo test --test commands -- script:: -- --test-threads=8
```

期望：有序集合命令和脚本里的 `ZADD` / `ZRANGEBYSCORE` 结果与改前一致，序列化字节不变。

### Task 2: 复用 Lua 虚拟机

**Files:**
- Modify: `src/command/script/sandbox.rs`
- Modify: `src/command/script/cache.rs`（增加 `generation`）
- Modify: `src/command/script.rs`（`execute_script` 改为借出虚拟机，不再每条 `new_sandbox_lua` + `drop`）
- Test: `tests/modules/command/script.rs`

**Interfaces:**
- Consumes: `new_sandbox_lua`，`harden_globals`，`ScriptCache`（最大 256，`EVAL` 不写入）
- Produces:
  - `ScriptCache::generation(&self) -> u64`。`insert` 挤掉旧项、`flush` 时加一。
  - `LuaPool::checkout(&self, generation) -> Result<PooledLua>`
  - `PooledLua`：`Deref` 到 `mlua::Lua`；`Drop` 时归还。归还前执行 `reset_for_reuse`。
  - `reset_for_reuse(lua)`：记下沙箱建好后的全局名字；把名单以外的全局设为 nil；去掉 `KEYS`、`ARGV`、`redis`。然后 `lua.gc_collect()`。
  - 池的容量是 `std::thread::available_parallelism()`，至少 1。池空时本次新建一个虚拟机，用完丢弃，不无限保留。虚拟机不能同时跑两段脚本，所以池是并发脚本的复用，不是全局一把锁。

`redis.call` 闭包捕获当次 `ScriptTransaction`，因此每条脚本仍然 `install_redis_api`。编译缓存只放 `SCRIPT LOAD` / `EVALSHA` 命中过的 SHA1，挂在该虚拟机的 registry 表 `aikv_sha` 上，条目数不超过 `ScriptCache` 的 256。借出时若 `generation` 与缓存不一致，清空 `aikv_sha`。普通 `EVAL` 每次 `load`，不把一次性脚本体放进 registry。

超时 hook 和 128MB 内存上限在虚拟机创建时设置一次。复用后重新 `set_hook`，因为 hook 闭包捕获了当次的 `Instant`。

- [x] **Step 1: 写失败测试**

在 `tests/modules/command/script.rs`：

- 第一条 EVAL 执行 `_G.leaked = KEYS[1]`，第二条 EVAL `return leaked`。期望 bulk nil，而不是第一条的 key。
- 复用之后 `return load` 仍然是脚本错误（`load` 保持封印）。
- `SCRIPT LOAD` 后连续两次 `EVALSHA` 结果相同。`SCRIPT FLUSH` 后再 `EVALSHA` 仍是 `NOSCRIPT`。
- 给 `LuaPool` 一个测试计数：池内虚拟机在两次顺序 EVAL 之后，`new_sandbox_lua` 的创建次数为 1。这个计数放在池上，不读取脚本正文。

- [x] **Step 2: 跑测试确认失败**

```bash
cargo test --test commands -- script:: -- --test-threads=8
```

期望：`LuaPool` 尚不存在，针对它的测试编译失败。

全局泄漏测试直接对同一个 `PooledLua` 连续跑两段脚本，不依赖 EVAL 碰巧新建虚拟机。今天的 `execute_script` 每次新建虚拟机，用它测不出泄漏。

- [x] **Step 3: 实现池和重置**

`execute_script` 在拿到键锁之后 `checkout`，`populate_keys_argv`，`install_redis_api`，`eval_async`，然后归还虚拟机，再 `commit`。脚本失败同样归还；归还路径走 `Drop`。

- [x] **Step 4: 跑测试**

```bash
cargo test --test commands -- script:: -- --test-threads=8
```

期望：沙箱封印、`NOSCRIPT`、全局不泄漏、顺序执行只创建一次虚拟机。现有脚本行为测试保持通过。

### Task 3: 相同声明 key 的脚本组提交

**Files:**
- Create: `src/command/script/commit_chain.rs`
- Modify: `src/command/script.rs`（`execute_script` 加入链，而不是自己持锁直到 `commit` 返回）
- Modify: `src/command/script/transaction.rs`（读路径先看本轮的 `BurstOverlay`）
- Test: `tests/modules/storage/script_commit_batch.rs`
- Modify: `docs/modules/05-commands-extended.md` 的 Lua 流程一句：相同 KEYS 的排队脚本可以合成一次 `apply_writes`；客户端在该次提交成功后才收到成功回复

**Interfaces:**
- Consumes: `KeyLock::lock_keys_sorted_with_timeout`，`ScriptTransaction::commit`，`KvStorage::apply_stored_batch`，`SET_BATCH_MAX_OPS`（512）
- Produces:
  - `CommitChain::enter(keys, job) -> ChainTicket`
  - `ChainTicket::run(self) -> Result<()>`：只有 owner 调用。joiner 的 `enter` 直接等待 owner 完成，自己不加锁。
  - `BurstOverlay`：`get(key) -> Option<OverlayEntry>`，`apply_script(write_set)`，`into_batch() -> Vec<(Vec<u8>, StoredBatchOp)>`
  - 链的身份是声明 key 的字节序列完全相同（顺序按 `lock_keys_sorted` 使用的桶序规范化）。key 集合不同的脚本不进同一条链，仍按今天的键锁并行。

Owner 的状态机：

1. `enter` 时若该 key 集合已有 owner，把 job 放进队列并返回等待。队列里的 job 不拿键锁。
2. 没有 owner 时，自己成为 owner，按 30 秒超时拿键锁。拿不到锁则这一条脚本返回现有的锁超时错误，不带动别人。
3. 持锁执行自己的脚本。Lua 失败则这条 job 返回该错误，丢弃它的写缓冲，与今天 `commit` 之前 drop 一样。
4. 再从队列取出已经在等待的、同一 key 集合的 job，放进本轮，直到本轮写操作数达到 `SET_BATCH_MAX_OPS`。不 `sleep`。没有等待者就只提交已经执行完的脚本。
5. 后一条脚本的 `fetch_stored` / `get` / `exists` 先读 `BurstOverlay`，再读自己的 `write_buffer` 和存储。因此它看得到本轮里前面成功脚本的写。
6. 本轮有写时调用一次 `apply_stored_batch`。没有写则不调用。成功后回复本轮每一条脚本（包括只读、但读过本轮未提交写的脚本）。
7. `apply_stored_batch` 失败：本轮全部 job 返回这个错误，overlay 丢弃，存储保持提交前的内容。不回复部分成功。
8. 队列里超出 512 笔的 job 不在这一轮执行。owner 提交并回复本轮之后释放键锁，把 owner 身份交给队首，由它重新拿锁。这样插在中间的普通 `SET` 能在两轮脚本之间拿到锁。
9. 不同 key 集合的 EVAL 不互相等待，除非它们的键锁桶真的重叠；重叠时后者堵在键锁上，这是现有语义。

内存引擎没有 Raft。`apply_stored_batch` 的默认实现仍逐条写本地。组提交对它的可见行为是：同一链上的多条脚本只触发一次 `apply_stored_batch`，读己之写仍然成立。

- [x] **Step 1: 写失败测试**

扩展 `tests/modules/storage/script_commit_batch.rs` 的 `CountingAdapter`：

- 两条 EVAL 声明同一把 key，第一条 `SET`，第二条 `GET` 并再 `SET` 另一个值。用一个屏障让第二条在第一条还没 `commit` 时已经进入 `CommitChain`。断言 `apply_writes == 1`，第二条读到第一条写入的值，最终存储是第二条的值。
- 两条 EVAL 声明不同的 key。断言它们可以各一次 `apply_writes`（允许因为调度合成一次，但不得要求必须合成；断言每把 key 的最终值正确，且不存在跨 key 的读己之写）。
- 适配器第一次 `apply_writes` 返回错误。同一条链上两条都已执行的 EVAL 都返回错误，`GET` 看不到其中任何写入。
- 一条只有 `return 1` 的脚本：`apply_writes` 仍为 0。
- 预先写入 600 个不同 field 的脚本不要写进测试。用 600 次单 key `SET` 脚本排队：断言任意一次 `apply_writes` 的 op 数不超过 512，且 600 个值最终都能读回。这覆盖的是上限，不是某个外部脚本。

- [x] **Step 2: 跑测试确认失败**

```bash
cargo test --test storage script_commit_batch -- --test-threads=8
```

期望：同一 key 的两条脚本今天是两次 `apply_writes`，新断言失败。

- [x] **Step 3: 实现 CommitChain**

`execute_script` 改为 `CommitChain::enter`。owner 在持锁期间借出 `LuaPool` 的虚拟机执行本轮脚本。joiner 不借虚拟机，等 owner 帮它执行。

不要在 `GroupSetBatcher` 里增加等待时间。链已经把多条脚本的写收成一次 `apply_stored_batch`，现有 512 笔和 50 微秒逻辑原样处理这一次调用。

- [x] **Step 4: 跑测试**

```bash
cargo test --test storage script_commit_batch -- --test-threads=8
cargo test --test commands -- script:: -- --test-threads=8
```

期望：组提交测试通过；脚本命令测试通过；`MULTI/EXEC` 相关测试不在本任务修改范围内，若本次 diff 未改其文件则不跑全库。

### Task 4: 三阶段一起复核

**Files:**
- 无新文件

- [x] **Step 1: 格式和本计划涉及的测试**

```bash
cargo fmt --all -- --check
cargo test --test commands zset:: -- --test-threads=8
cargo test --test commands -- script:: -- --test-threads=8
cargo test --test storage script_commit_batch -- --test-threads=8
```

期望：格式通过；三个测试目标通过。

- [x] **Step 2: 对照约束再看一遍 diff**

确认 diff 里没有：测试名或 Hauler key 前缀分支、超时常量改动、`ValueType::ZSet` 布局改动、`MULTI/EXEC` 合并、新的凑批 sleep、`sync_wal` 默认值改动。
