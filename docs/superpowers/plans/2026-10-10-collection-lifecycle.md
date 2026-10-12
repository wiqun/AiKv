# 大集合生命周期与版本化 Subkey 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Hash/Set 的 subkey 布局在 DEL/过期/RENAME/MOVE/SWAPDB/DUMP/Lua 下都正确, 垃圾由 compaction 回收, 多 field 写只提交一次.

**Architecture:** header 带版本号, subkey 放进独立键空间并带上版本. 换版本的写先暂存 subkey 再切 header. aidb 的 compaction filter 增加点查句柄, aikv filter 据此删除版本不匹配的 subkey. 同时修复 filter 删除最新 Put 导致旧版本复活的问题.

**Tech Stack:** Rust, tokio, postcard, aidb (LSM, 本地路径 patch), aikv `KvStorage` / `StorageAdapter`.

**Spec:** `aikv/docs/superpowers/specs/2026-10-10-collection-lifecycle-design.md`

## Global Constraints

- 开发阶段无存量数据: 不写旧格式迁移, 不做兼容读.
- aikv 通过 `~/.cargo/config.toml` 的 `[patch."https://github.com/wiqun/AiDb.git"]` 使用本地 `/root/code/wiqun/database/aidb` 工作树; aidb 当前检出的分支就是 aikv 编译用的版本.
- 单文件不超过 800 行. `src/storage/adapter.rs` 已 1108 行, 本计划只减不增.
- Conventional Commits: 英文类型 + 中文描述. `fix:` 提交带 `Fixes #NN`; 回归测试函数上方 `///` 中文注释写现象、期望、Issue 编号.
- 每个提交步骤都要先给用户审阅, 获准后才执行 `git commit`; 永不 push.
- 集成测试带 `--test-threads=1`. 只跑与当前任务相关的测试目标.
- 内联阈值 `HASH_MAX_INLINE_FIELDS = 64`, `SET_MAX_INLINE_MEMBERS = 64`.
- 版本号 `max(last + 1, now_us << 11)`; GC 宽限期 `SUBKEY_GC_GRACE_US = 3_600_000_000`.
- Subkey 物理 key: `{db}\x01{user_key_len:u32 BE}{user_key}{version:u64 BE}{tag}{sub}`, tag `b'H'` / `b'S'`.

## 执行前需要用户提供

Issue 编号. 计划中以 `#P1` 这样的记号出现, 执行到对应任务前替换为真实编号:

| 记号 | 问题 |
|------|------|
| `#P1` | DEL 只删 header, 重建后旧 field 复活 |
| `#P2` | 过期只删 header |
| `#P3` | MOVE 丢失大集合数据 |
| `#P4` | SWAPDB 丢失大集合数据 |
| `#P5` | DUMP/RESTORE/MIGRATE 丢失大集合数据 |
| `#P6` | Lua 写回后旧 subkey 复活 |
| `#P7` | MemoryEngine 第 65 个 field 报错 |
| `#P8` | KEYS/SCAN 把 subkey 当 key 返回 |
| `#P9` | 多 field HSET 逐 field 读写 |
| `#P10` | subkey 读错误被吞掉 |
| `#P11` | HSCAN/SSCAN 游标越界 panic |
| `#P12` | 父 key 含 0x01 被拒绝 / field 限长 |
| `#P13` | SMOVE / S*STORE 不走阈值; S*STORE 空结果写入空集合 |
| `#A1` | aidb: filter 删除最新 Put 后旧版本复活 |

## 分支

- aidb: 从 `feat/observability-tuning` (HEAD `0930951`) 切出 `fix/compaction-filter-reader`.
- aikv: 从 `perf/zset-read-decode` (HEAD `8d74287`, 含未提交的 ZSET 解码优化) 切出 `fix/collection-lifecycle`.

## 文件结构

| 文件 | 职责 |
|------|------|
| aidb `src/engine/compaction/filter.rs` | 新增 `CompactionPointReader`, `filter_with_reader` 默认方法, 契约文档 |
| aidb `src/engine/compaction/job.rs` | `FilterCtx`, `run_with_reader`, Remove→删除标记 |
| aidb `src/engine/db/inner/compaction.rs` | `DbPointReader`, 调用 `run_with_reader` |
| aikv `src/storage/subkey.rs` | 重写: 版本化 subkey 编解码, `routing_user_key` |
| aikv `src/storage/collection.rs` (新) | 阈值、版本号生成、`CollectionWrite` / `HeaderWrite` |
| aikv `src/storage/adapter/collection_ops.rs` (新) | `KvStorageAdapter` 的集合实现与跨 key 迁移 (从 adapter.rs 迁出) |
| aikv `src/storage/types.rs` | header 加 version, `KvStorage` 新方法, 删除 `raw_subkey_*` |
| aikv `src/storage/compaction_filter.rs` (由 `ttl_filter.rs` 改名) | `AikvCompactionFilter` |
| aikv `src/storage/cluster_adapter.rs` | 路由改用 `routing_user_key` |
| aikv `src/command/hash.rs`, `set.rs` | 改用新 API |
| aikv `src/command/{key.rs, database.rs, router/mod.rs, script/transaction.rs}` | DUMP/RESTORE/MOVE/OBJECT/Lua |
| aikv `tests/modules/storage/map_adapter.rs` (新) | 共享内存 `StorageAdapter`, 统计 `apply_writes` 次数 |
| aikv `tests/modules/storage/collection_lifecycle.rs` (新) | 回归测试, 双引擎 |
| aikv `tests/modules/storage/subkey_gc.rs` (新) | 真实 aidb 上的 GC 测试 |

---

### Task 0: 分支与已完成的 ZSET 解码优化

**Files:**
- Modify: 无 (提交已有改动 `src/storage/types.rs`, `src/command/zset/mod.rs`)

- [ ] **Step 1: 建分支**

```bash
cd /root/code/wiqun/database/aidb && git switch -c fix/compaction-filter-reader
cd /root/code/wiqun/database/aikv && git switch -c fix/collection-lifecycle
```

- [ ] **Step 2: 去掉 `deserialize_zset_members` 注释里的 Issue 待填说明**

在 `src/storage/types.rs` 找到 `deserialize_zset_members` 上方注释, 若含 "Issue" 待填字样则删去该半句 (这是 `perf:` 提交, 不要求 Issue).

- [ ] **Step 3: 跑相关测试**

Run: `cargo test --lib storage::types::tests::zset_bulk_decode_keeps_wire_format && cargo test --test commands -- zset:: --test-threads=1`
Expected: PASS

- [ ] **Step 4: 提交 (需用户审阅)**

```bash
git add src/storage/types.rs src/command/zset/mod.rs
git commit -m "perf: ZSET 读取时批量解码并避免整值克隆"
```

---

### Task 1: aidb compaction filter 点查句柄

**Files:**
- Modify: `aidb/src/engine/compaction/filter.rs`
- Modify: `aidb/src/engine/compaction/mod.rs:13`
- Modify: `aidb/src/engine/compaction/job.rs` (`should_filter_impl`, `write_sub_compaction`, `run`, `run_single`, `run_split`, `should_filter`)
- Modify: `aidb/src/engine/db/inner/compaction.rs:114-127`
- Test: `aidb/src/engine/compaction/job.rs` tests 模块; `aidb/tests/proptest/compaction_filter.rs`
- Docs: `aidb/docs/modules/02-engine-storage.md`, `aidb/CHANGELOG.md`

**Interfaces:**
- Produces:
  - `pub trait CompactionPointReader: Send + Sync { fn get(&self, user_key: &[u8]) -> crate::Result<Option<Vec<u8>>>; }`
  - `CompactionFilter::filter_with_reader(&self, level: usize, key: &[u8], value: &[u8], reader: &dyn CompactionPointReader) -> FilterDecision` (默认转发 `filter`)
  - `CompactionJob::run_with_reader(&self, file_numbers: &[u64], reader: Option<&dyn CompactionPointReader>) -> Result<Vec<CompactionResult>>`
  - 导出路径 `aidb::engine::compaction::CompactionPointReader`

- [ ] **Step 1: 写失败的单元测试 (job.rs tests 模块末尾)**

```rust
    #[test]
    fn test_filter_with_reader_sees_reader() {
        use crate::engine::compaction::{CompactionFilter, CompactionPointReader, FilterDecision};

        struct ParentReader;
        impl CompactionPointReader for ParentReader {
            fn get(&self, user_key: &[u8]) -> crate::Result<Option<Vec<u8>>> {
                Ok((user_key == b"live").then(|| b"1".to_vec()))
            }
        }

        /// key 形如 "{parent}/x": parent 不存在就删除.
        struct DropOrphans;
        impl CompactionFilter for DropOrphans {
            fn filter(&self, _: usize, _: &[u8], _: &[u8]) -> FilterDecision {
                FilterDecision::Keep
            }
            fn filter_with_reader(
                &self,
                _: usize,
                key: &[u8],
                _: &[u8],
                reader: &dyn CompactionPointReader,
            ) -> FilterDecision {
                let user_key = crate::engine::memtable::extract_user_key(key);
                let parent = user_key.split(|b| *b == b'/').next().unwrap_or_default();
                match reader.get(parent) {
                    Ok(None) => FilterDecision::Remove,
                    _ => FilterDecision::Keep,
                }
            }
        }

        let dir = tempdir().unwrap();
        let a = sst(
            dir.path(),
            1,
            0,
            &[
                (b"dead/x", 2, ValueType::TypePut, b"v"),
                (b"live/x", 1, ValueType::TypePut, b"v"),
            ],
        );
        let job = CompactionJob::new(
            vec![a],
            vec![],
            1,
            dir.path().to_path_buf(),
            512,
            16,
            CompressionType::None,
            0.0,
        )
        .with_filter(Some(Arc::new(DropOrphans)));

        let without_reader = job.run(&[30]).unwrap();
        assert_eq!(without_reader[0].entry_count, 2);

        let with_reader = job.run_with_reader(&[31], Some(&ParentReader)).unwrap();
        assert_eq!(with_reader[0].entry_count, 1);
        let reader = SSTableReader::open(&with_reader[0].output_path, None).unwrap();
        let seek = encode_internal_key(b"live/x", u64::MAX, ValueType::TypePut);
        assert!(reader.get(&seek).unwrap().is_some());
    }
```

- [ ] **Step 2: 确认失败**

Run: `cd /root/code/wiqun/database/aidb && cargo test --lib engine::compaction::job::tests::test_filter_with_reader_sees_reader`
Expected: 编译失败, `CompactionPointReader` / `run_with_reader` 未定义.

- [ ] **Step 3: filter.rs 增加 trait 并改契约文档**

模块文档 "副作用边界" 第一条改为:

```rust
//! - [`CompactionFilter`] 不得写入引擎或修改全局状态. 需要读其它 key 时只能用
//!   [`CompactionFilter::filter_with_reader`] 拿到的 [`CompactionPointReader`] 点查.
```

trait 文档中 "**无副作用**: 不应修改全局状态或发起 I/O." 改为 "**无写副作用**: 不修改全局状态; 读其它 key 只能经 `filter_with_reader` 的 reader.". 在 `CompactionFilter` 前加:

```rust
/// compaction 期间按最新可见版本点查 user key 的只读句柄.
///
/// 由 `DB::run_compaction_once` 提供; 不计入 Get 操作统计.
pub trait CompactionPointReader: Send + Sync {
    fn get(&self, user_key: &[u8]) -> crate::Result<Option<Vec<u8>>>;
}
```

`CompactionFilter` 内追加:

```rust
    /// 带点查句柄的过滤. 默认忽略 reader, 转发 [`CompactionFilter::filter`].
    ///
    /// compaction 估算路径没有 reader, 只会调用 `filter`; 两者对同一 entry 的
    /// 结论可以不同, 但 `filter` 必须更保守 (只在确定可删时返回 `Remove`).
    fn filter_with_reader(
        &self,
        level: usize,
        key: &[u8],
        value: &[u8],
        reader: &dyn CompactionPointReader,
    ) -> FilterDecision {
        let _ = reader;
        self.filter(level, key, value)
    }
```

`mod.rs:13` 改为 `pub use filter::{CompactionFilter, CompactionPointReader, CompactionRemovalListener, FilterDecision};`

- [ ] **Step 4: job.rs 透传 reader**

新增:

```rust
/// filter 与可选点查句柄, 随子任务一起下发.
#[derive(Clone, Copy)]
struct FilterCtx<'a> {
    filter: Option<&'a Arc<dyn CompactionFilter>>,
    reader: Option<&'a dyn CompactionPointReader>,
}

fn should_filter_impl(ctx: FilterCtx<'_>, level: usize, key: &[u8], value: &[u8]) -> bool {
    let Some(f) = ctx.filter else {
        return false;
    };
    let decision = match ctx.reader {
        Some(reader) => f.filter_with_reader(level, key, value, reader),
        None => f.filter(level, key, value),
    };
    decision == FilterDecision::Remove
}
```

- `write_sub_compaction` 的参数 `compaction_filter: Option<Arc<dyn CompactionFilter>>` 换成 `ctx: FilterCtx<'_>`, 内部三处 `should_filter_impl(&compaction_filter, ...)` 改为 `should_filter_impl(ctx, ...)`.
- `pub fn run(&self, file_numbers)` 改为 `self.run_with_reader(file_numbers, None)`; 原 `run` 的函数体移入 `pub fn run_with_reader(&self, file_numbers: &[u64], reader: Option<&dyn CompactionPointReader>)`, 并把 `reader` 传给 `run_single(fnum, reader)` / `run_split(file_numbers, reader)`.
- `run_single` / `run_split` 用 `FilterCtx { filter: self.compaction_filter.as_ref(), reader }` 调 `write_sub_compaction`. `run_split` 中 `std::thread::scope` 内闭包直接捕获 `ctx` (它是 `Copy`, 借用在 scope 内有效), 删除原来逐线程 `compaction_filter.clone()`.
- `fn should_filter(&self, key, value)` (估算路径) 改为 `should_filter_impl(FilterCtx { filter: self.compaction_filter.as_ref(), reader: None }, self.output_level, key, value)`.
- import 增加 `CompactionPointReader`.

- [ ] **Step 5: 跑单元测试**

Run: `cargo test --lib engine::compaction::job`
Expected: PASS (含新测试与已有 6 个测试)

- [ ] **Step 6: 写失败的 DB 级测试 (`tests/proptest/compaction_filter.rs` 末尾, proptest! 宏之外)**

```rust
/// header 形如 "h{n}", 子项形如 "s{n}/..."; header 不存在的子项在 compaction 中被删除.
struct OrphanFilter;

impl CompactionFilter for OrphanFilter {
    fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> FilterDecision {
        FilterDecision::Keep
    }
    fn filter_with_reader(
        &self,
        _level: usize,
        key: &[u8],
        _value: &[u8],
        reader: &dyn aidb::engine::compaction::CompactionPointReader,
    ) -> FilterDecision {
        let user_key = aidb::engine::memtable::extract_user_key(key);
        let Some(rest) = user_key.strip_prefix(b"s") else {
            return FilterDecision::Keep;
        };
        let n = rest.split(|b| *b == b'/').next().unwrap_or_default();
        let header = [b"h".as_slice(), n].concat();
        match reader.get(&header) {
            Ok(None) => FilterDecision::Remove,
            _ => FilterDecision::Keep,
        }
    }
}

#[test]
fn filter_with_reader_drops_orphans_during_db_compaction() {
    let dir = tempdir().unwrap();
    let db = Arc::new(DB::open(dir.path(), test_opts()).unwrap());
    db.set_compaction_filter(Some(Arc::new(OrphanFilter)));

    db.put(b"h1", b"x").unwrap();
    for i in 0..20u8 {
        db.put(&[b"s1/".as_slice(), &[i]].concat(), b"v").unwrap();
        db.put(&[b"s2/".as_slice(), &[i]].concat(), b"v").unwrap();
    }
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'p', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();

    for i in 0..20u8 {
        assert!(db.get(&[b"s1/".as_slice(), &[i]].concat()).unwrap().is_some());
        assert!(db.get(&[b"s2/".as_slice(), &[i]].concat()).unwrap().is_none());
    }
    db.close().unwrap();
}
```

- [ ] **Step 7: 确认失败**

Run: `cargo test --test proptest compaction_filter::filter_with_reader_drops_orphans_during_db_compaction -- --test-threads=1`
Expected: FAIL, `s2/*` 仍存在 (DB 还没传 reader).

- [ ] **Step 8: inner/compaction.rs 传入 reader**

在文件内 `impl DB` 之外新增:

```rust
/// 借用 `&DB` 的点查句柄. 只在 `run_compaction_once` 栈上存活.
struct DbPointReader<'a> {
    db: &'a DB,
}

impl CompactionPointReader for DbPointReader<'_> {
    fn get(&self, user_key: &[u8]) -> Result<Option<Vec<u8>>> {
        let max_seq = self.db.sequence.load(AtomicOrdering::SeqCst);
        self.db.get_at_sequence(user_key, max_seq)
    }
}
```

`CompactionJob::new(...)...with_filter(...).run(&file_numbers)?` 改为:

```rust
        let reader = DbPointReader { db: self };
        let results = CompactionJob::new(
            // 参数不变
        )
        .with_snapshot_threshold(min_snap_seq)
        .with_surviving_user_ranges(surviving)
        .with_filter(self.compaction_filter.read().clone())
        .run_with_reader(&file_numbers, Some(&reader))?;
```

核对: 这一调用发生在 `min_snap_seq` 块的 `write_lock` guard 释放之后、`sstables.write()` 之前; `get_at_sequence` 只取 `memtable.read()` 与 `sstables.read()`, 不会与本函数持锁冲突. 用 `rg -n "run_with_reader|\.run\(" src/engine/db` 确认只有这一处调用.

- [ ] **Step 9: 跑测试**

Run: `cargo test --test proptest compaction_filter -- --test-threads=1 && cargo test --lib engine::compaction`
Expected: PASS

- [ ] **Step 10: 文档**

- `docs/modules/02-engine-storage.md` 的 CompactionFilter 段落: 契约改为 "可经 reader 点查, 不得写入"; 说明 `CompactionPointReader` 由 `run_compaction_once` 提供、读最新可见版本、估算路径不带 reader.
- `CHANGELOG.md` `[Unreleased]` → Added: "CompactionFilter 新增 `filter_with_reader`, 可在 compaction 中点查其它 key (`CompactionPointReader`)".

- [ ] **Step 11: 提交 (需用户审阅)**

```bash
git add src/engine/compaction src/engine/db/inner/compaction.rs tests/proptest/compaction_filter.rs docs/modules/02-engine-storage.md CHANGELOG.md
git commit -m "feat: CompactionFilter 支持在 compaction 中点查引擎"
```

---

### Task 2: aidb filter 删除最新 Put 时保留删除标记 (新发现, 需用户确认纳入)

**Files:**
- Modify: `aidb/src/engine/compaction/job.rs` (`write_sub_compaction` 最新版本分支, 约 573-578 行)
- Modify: `aidb/src/engine/compaction/drop.rs` (新增 `overlaps_surviving`)
- Test: `aidb/src/engine/compaction/job.rs` tests 模块
- Docs: `aidb/docs/modules/02-engine-storage.md`, `aidb/CHANGELOG.md`

**Interfaces:**
- Produces: `pub fn overlaps_surviving(user_key: &[u8], surviving: &[UserKeyRange]) -> bool` (drop.rs)

- [ ] **Step 1: 写失败的单元测试**

```rust
    /// filter 删除最新 Put 时, 若更深层仍有同 key 旧版本, 必须写删除标记; 否则旧值复活.
    /// 期望: 输出中 k 为删除标记. Issue #A1
    #[test]
    fn test_filter_remove_over_surviving_writes_tombstone() {
        use crate::engine::compaction::{CompactionFilter, FilterDecision};

        struct DropAll;
        impl CompactionFilter for DropAll {
            fn filter(&self, _: usize, _: &[u8], _: &[u8]) -> FilterDecision {
                FilterDecision::Remove
            }
        }

        let dir = tempdir().unwrap();
        let a = sst(dir.path(), 1, 0, &[(b"k", 5, ValueType::TypePut, b"new")]);
        let job = CompactionJob::new(
            vec![a],
            vec![],
            1,
            dir.path().to_path_buf(),
            512,
            16,
            CompressionType::None,
            0.0,
        )
        .with_filter(Some(Arc::new(DropAll)))
        .with_surviving_user_ranges(vec![UserKeyRange {
            start: b"a".to_vec(),
            end: b"z".to_vec(),
        }]);
        let out = job.run(&[40]).unwrap();
        assert_eq!(out[0].entry_count, 1);
        assert_eq!(out[0].filter_removed_user_keys, vec![b"k".to_vec()]);
        let reader = SSTableReader::open(&out[0].output_path, None).unwrap();
        let seek = encode_internal_key(b"k", u64::MAX, ValueType::TypePut);
        let (_, ty) = reader.get(&seek).unwrap().expect("tombstone");
        assert_eq!(ty, ValueType::TypeDelete);
    }
```

已有的 `test_filter_removed_user_keys_recorded` (无 surviving, 无快照) 期望 `entry_count == 0`, 保持不变, 用于确认没有多写删除标记.

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib engine::compaction::job::tests::test_filter_remove_over_surviving_writes_tombstone`
Expected: FAIL, `entry_count` 为 0. (`SSTableReader::get` 返回 `Result<Option<(Vec<u8>, ValueType)>>`.)

- [ ] **Step 3: drop.rs 抽出点重叠判断**

```rust
/// 点 key 是否落在任一幸存文件的区间内.
pub fn overlaps_surviving(user_key: &[u8], surviving: &[UserKeyRange]) -> bool {
    surviving
        .iter()
        .any(|file| marker_overlaps(user_key, None, file))
}
```

- [ ] **Step 4: job.rs 改写最新版本分支**

把

```rust
        if !should_filter_impl(ctx, output_level, &key, &value) {
            builder.add(&key, &value)?;
            entry_count += 1;
        } else if value_type == ValueType::TypePut {
            filter_removed.push(user_key.to_vec());
        }
```

改为

```rust
        if !should_filter_impl(ctx, output_level, &key, &value) {
            builder.add(&key, &value)?;
            entry_count += 1;
        } else if value_type == ValueType::TypePut {
            filter_removed.push(user_key.to_vec());
            // 更老的版本还在快照或幸存文件里时, 不留标记它就会重新可见.
            if min_snap_seq != u64::MAX || overlaps_surviving(user_key, &surviving) {
                let marker = encode_internal_key(user_key, seq, ValueType::TypeDelete);
                builder.add(&marker, &[])?;
                entry_count += 1;
                if smallest_key.is_none() {
                    smallest_key = Some(marker.clone());
                }
                largest_key = Some(marker);
                last_user_key = Some(user_key.to_vec());
                tracker.observe(seq, min_snap_seq);
                continue;
            }
        }
```

import 增加 `encode_internal_key` (`crate::engine::memtable`) 与 `overlaps_surviving`.

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib engine::compaction && cargo test --test proptest compaction_filter -- --test-threads=1`
Expected: PASS

- [ ] **Step 6: 文档**

- `02-engine-storage.md`: filter 删除最新 Put 且存在快照或幸存重叠时写同 sequence 删除标记.
- `CHANGELOG.md` → Fixed: "compaction filter 删除最新版本后, 更深层的旧版本不再复活".

- [ ] **Step 7: 提交 (需用户审阅)**

```bash
git add src/engine/compaction docs/modules/02-engine-storage.md CHANGELOG.md
git commit -m "fix: compaction filter 删除最新版本时保留删除标记防止旧值复活

Fixes #A1"
```

---

### Task 3: aikv 版本化 subkey 编码、版本号与集群路由

**Files:**
- Modify: `aikv/src/storage/subkey.rs` (新函数与旧函数并存, 旧的在 Task 9 删除)
- Create: `aikv/src/storage/collection.rs`
- Modify: `aikv/src/storage/mod.rs` (声明 `pub mod collection;`)
- Modify: `aikv/src/storage/cluster_adapter.rs:226-276,339-346`
- Test: `subkey.rs` / `collection.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Produces (subkey.rs):
  - `pub fn kind_tag(kind: CollectionKind) -> u8`
  - `pub fn subkey_db_prefix(db: usize) -> Vec<u8>` → `{db}\x01`
  - `pub fn subkey_version_prefix(db: usize, user_key: &[u8], version: u64, kind: CollectionKind) -> Vec<u8>`
  - `pub fn encode_subkey(db: usize, user_key: &[u8], version: u64, kind: CollectionKind, sub: &[u8]) -> Vec<u8>`
  - `pub struct DecodedSubkey<'a> { pub db: usize, pub user_key: &'a [u8], pub version: u64, pub kind: CollectionKind, pub sub: &'a [u8] }`
  - `pub fn decode_versioned_subkey(physical: &[u8]) -> Option<DecodedSubkey<'_>>`
  - `pub fn routing_user_key(physical: &[u8]) -> Option<Vec<u8>>`
- Produces (collection.rs):
  - `pub const HASH_MAX_INLINE_FIELDS: usize = 64;` `pub const SET_MAX_INLINE_MEMBERS: usize = 64;`
  - `pub const SUBKEY_GC_GRACE_US: u64 = 3_600_000_000;`
  - `pub fn next_collection_version() -> u64` / `pub fn version_micros(version: u64) -> u64` / `pub fn now_micros() -> u64`

- [ ] **Step 1: 写失败的单元测试 (subkey.rs tests 模块追加)**

```rust
    #[test]
    fn versioned_subkey_roundtrip_with_any_key_bytes() {
        let key = b"a\x01b:c";
        let encoded = encode_subkey(3, key, 42, CollectionKind::Hash, b"f\x00ield");
        let decoded = decode_versioned_subkey(&encoded).expect("decode");
        assert_eq!(decoded.db, 3);
        assert_eq!(decoded.user_key, key);
        assert_eq!(decoded.version, 42);
        assert_eq!(decoded.kind, CollectionKind::Hash);
        assert_eq!(decoded.sub, b"f\x00ield");
    }

    #[test]
    fn versioned_subkeys_never_share_the_main_key_prefix() {
        let sub = encode_subkey(0, b"k", 1, CollectionKind::Set, b"m");
        assert!(!sub.starts_with(b"0:"));
        assert!(sub.starts_with(&subkey_db_prefix(0)));
        assert!(!encode_subkey(10, b"k", 1, CollectionKind::Set, b"m")
            .starts_with(&subkey_db_prefix(1)));
    }

    #[test]
    fn version_prefix_isolates_versions() {
        let p1 = subkey_version_prefix(0, b"k", 1, CollectionKind::Hash);
        let v2 = encode_subkey(0, b"k", 2, CollectionKind::Hash, b"f");
        assert!(!v2.starts_with(&p1));
        let other = encode_subkey(0, b"kk", 1, CollectionKind::Hash, b"f");
        assert!(!other.starts_with(&p1));
    }

    #[test]
    fn main_keys_are_not_versioned_subkeys() {
        assert!(decode_versioned_subkey(b"0:a\x01b").is_none());
        assert!(decode_versioned_subkey(b"0:plain").is_none());
    }

    #[test]
    fn routing_key_matches_parent_for_both_spaces() {
        let sub = encode_subkey(0, b"{tag}k", 9, CollectionKind::Hash, b"f");
        assert_eq!(routing_user_key(&sub).as_deref(), Some(b"{tag}k".as_ref()));
        assert_eq!(routing_user_key(b"0:{tag}k").as_deref(), Some(b"{tag}k".as_ref()));
    }
```

collection.rs 测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_strictly_increasing_and_carry_time() {
        let before = now_micros();
        let a = next_collection_version();
        let b = next_collection_version();
        assert!(b > a);
        assert!(version_micros(a) >= before);
        assert!(version_micros(b) <= now_micros());
    }
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib storage::subkey storage::collection`
Expected: 编译失败, 新函数未定义. (cargo 只接受一个过滤串; 分两次运行: `cargo test --lib storage::subkey` 与 `cargo test --lib storage::collection`.)

- [ ] **Step 3: 实现 subkey.rs 新函数**

```rust
/// 版本化 subkey 键空间标记: `{db}\x01...`, 与主键空间 `{db}:` 不共享前缀.
pub const SUBKEY_SPACE: u8 = 0x01;

pub fn kind_tag(kind: CollectionKind) -> u8 {
    match kind {
        CollectionKind::Hash => HASH_TAG,
        CollectionKind::Set => SET_TAG,
    }
}

fn kind_from_tag(tag: u8) -> Option<CollectionKind> {
    match tag {
        HASH_TAG => Some(CollectionKind::Hash),
        SET_TAG => Some(CollectionKind::Set),
        _ => None,
    }
}

pub fn subkey_db_prefix(db: usize) -> Vec<u8> {
    let mut prefix = db.to_string().into_bytes();
    prefix.push(SUBKEY_SPACE);
    prefix
}

pub fn subkey_version_prefix(
    db: usize,
    user_key: &[u8],
    version: u64,
    kind: CollectionKind,
) -> Vec<u8> {
    let mut prefix = subkey_db_prefix(db);
    prefix.reserve(4 + user_key.len() + 9);
    prefix.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    prefix.extend_from_slice(user_key);
    prefix.extend_from_slice(&version.to_be_bytes());
    prefix.push(kind_tag(kind));
    prefix
}

pub fn encode_subkey(
    db: usize,
    user_key: &[u8],
    version: u64,
    kind: CollectionKind,
    sub: &[u8],
) -> Vec<u8> {
    let mut key = subkey_version_prefix(db, user_key, version, kind);
    key.extend_from_slice(sub);
    key
}

#[derive(Debug, PartialEq, Eq)]
pub struct DecodedSubkey<'a> {
    pub db: usize,
    pub user_key: &'a [u8],
    pub version: u64,
    pub kind: CollectionKind,
    pub sub: &'a [u8],
}

pub fn decode_versioned_subkey(physical: &[u8]) -> Option<DecodedSubkey<'_>> {
    let sep = physical.iter().position(|&b| b == SUBKEY_SPACE)?;
    let digits = &physical[..sep];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let db = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let rest = &physical[sep + 1..];
    let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
    let rest = &rest[4..];
    let user_key = rest.get(..len)?;
    let rest = &rest[len..];
    let version = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
    let kind = kind_from_tag(*rest.get(8)?)?;
    Some(DecodedSubkey {
        db,
        user_key,
        version,
        kind,
        sub: &rest[9..],
    })
}

/// 集群路由用的 user key: subkey 取父 key, 主键取 `{db}:` 之后的部分.
pub fn routing_user_key(physical: &[u8]) -> Option<Vec<u8>> {
    if let Some(decoded) = decode_versioned_subkey(physical) {
        return Some(decoded.user_key.to_vec());
    }
    crate::storage::AiDbEngine::decode_key(physical).map(|(_, key)| key)
}
```

- [ ] **Step 4: 新建 collection.rs**

```rust
//! 大集合 (subkey 布局) 的阈值、版本号与写入描述.
//!
//! 版本号 = max(上次 + 1, 当前微秒 << 11). 低 11 位是同一微秒内的序号,
//! `version >> 11` 是生成时刻, compaction GC 用它判断宽限期.
//! 时钟回拨后新旧版本可能相等, 依赖 NTP; 见 docs/modules/03-storage.md.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const HASH_MAX_INLINE_FIELDS: usize = 64;
pub const SET_MAX_INLINE_MEMBERS: usize = 64;

/// 暂存写到切换 header 之间, subkey 看起来是垃圾; 这段时间内 GC 不碰它.
pub const SUBKEY_GC_GRACE_US: u64 = 3_600_000_000;

const SEQ_BITS: u32 = 11;

static LAST_VERSION: AtomicU64 = AtomicU64::new(0);

pub fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

pub fn next_collection_version() -> u64 {
    let floor = now_micros() << SEQ_BITS;
    let mut last = LAST_VERSION.load(Ordering::Relaxed);
    loop {
        let next = floor.max(last + 1);
        match LAST_VERSION.compare_exchange_weak(last, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

pub fn version_micros(version: u64) -> u64 {
    version >> SEQ_BITS
}
```

`src/storage/mod.rs` 加 `pub mod collection;`.

- [ ] **Step 5: 集群路由改用 `routing_user_key`**

`cluster_adapter.rs`:
- 删除 `strip_subkey_suffix`.
- `route_read` / `route_write` 中

```rust
        let Some((_, user_key)) = AiDbEngine::decode_key(key) else {
            return Ok(None);
        };
        let routing_key = Self::strip_subkey_suffix(&user_key);
```

改为

```rust
        let Some(routing_key) = subkey::routing_user_key(key) else {
            return Ok(None);
        };
        let routing_key = routing_key.as_slice();
```

- `key_slot_status` 中 `let (_, user_key) = AiDbEngine::decode_key(key)?;` 改为 `let user_key = subkey::routing_user_key(key)?;`.
- 文件头 `use crate::storage::subkey;`.

- [ ] **Step 6: 跑测试与集群编译**

Run: `cargo test --lib storage::subkey && cargo test --lib storage::collection && cargo build --features cluster`
Expected: PASS, 编译通过

- [ ] **Step 7: 提交 (需用户审阅)**

```bash
git add src/storage/subkey.rs src/storage/collection.rs src/storage/mod.rs src/storage/cluster_adapter.rs
git commit -m "feat: 版本化 subkey 编码与集合版本号, 集群路由识别新 subkey"
```

---

### Task 4: 集合存储 API (header 版本、提交、暂存写、展开)

**Files:**
- Modify: `aikv/src/storage/types.rs` (`ValueType::CollectionHeader`, `KvStorage` trait 新方法与默认实现, `rewrite_stored` 默认实现)
- Modify: `aikv/src/storage/collection.rs` (追加 `CollectionWrite`, `HeaderWrite`)
- Create: `aikv/src/storage/adapter/collection_ops.rs`
- Modify: `aikv/src/storage/adapter.rs` (声明子模块, `KvStorage` impl 委托, `clear_db` 删两段)
- Modify: `aikv/src/command/hash.rs`, `set.rs` (仅为编译: header 构造补 `version`, `count` 改 u64; 逻辑在 Task 6/7 重写)
- Create: `aikv/tests/modules/storage/map_adapter.rs`
- Create: `aikv/tests/modules/storage/collection_lifecycle.rs`
- Modify: `aikv/tests/modules/storage/mod.rs`

**Interfaces:**
- Consumes: Task 3 全部.
- Produces:
  - `ValueType::CollectionHeader { kind: CollectionKind, count: u64, version: u64 }`
  - `pub enum HeaderWrite { Put(StoredValue), Delete { had_ttl: bool } }`
  - `pub struct CollectionWrite { pub header: HeaderWrite, pub version: u64, pub kind: CollectionKind, pub puts: Vec<(Vec<u8>, Vec<u8>)>, pub deletes: Vec<Vec<u8>> }`
  - `KvStorage` 新方法:
    - `fn supports_subkeys(&self) -> bool` (默认 `false`)
    - `async fn subkey_get_many(&self, db: usize, key: &[u8], version: u64, kind: CollectionKind, subs: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>>`
    - `async fn subkey_scan(&self, db: usize, key: &[u8], version: u64, kind: CollectionKind) -> Result<Vec<(Vec<u8>, Vec<u8>)>>` (按 sub 字节序)
    - `async fn commit_collection(&self, db: usize, key: &[u8], write: CollectionWrite) -> Result<()>`
    - `async fn put_value(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()>` (默认 `set_typed`)
    - `async fn get_expanded(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>>` (默认 `get_typed`)
  - `rewrite_stored` 默认实现改为调用 `put_value` (TTL 计数语义不变).
  - 测试辅助 `MapAdapter::new()`, `MapAdapter::apply_calls() -> usize`, `MapAdapter::keys_with_prefix(&[u8]) -> Vec<Vec<u8>>`.

- [ ] **Step 1: 抽出 MapAdapter (测试辅助)**

把 `tests/modules/storage/rename_subkey.rs` 里的 `MapAdapter` 移到 `tests/modules/storage/map_adapter.rs`, 改为 `BTreeMap` 存储 (保证 `scan_prefix` 有序), 并增加计数:

```rust
//! 测试用内存 `StorageAdapter`: 有序存储, 统计 `apply_writes` 调用次数.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use aikv::error::Result;
use aikv::storage::adapter::{AdapterWriteOp, StorageAdapter};
use async_trait::async_trait;

#[derive(Default)]
pub struct MapAdapter {
    data: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
    apply_calls: AtomicUsize,
}

impl MapAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_calls(&self) -> usize {
        self.apply_calls.load(Ordering::SeqCst)
    }

    pub fn keys_with_prefix(&self, prefix: &[u8]) -> Vec<Vec<u8>> {
        self.data
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect()
    }
}

#[async_trait]
impl StorageAdapter for MapAdapter {
    async fn get(&self, key: Vec<u8>) -> Result<Option<Vec<u8>>> {
        Ok(self.data.lock().unwrap().get(&key).cloned())
    }

    async fn set(&self, key: Vec<u8>, value: Vec<u8>) -> Result<()> {
        self.data.lock().unwrap().insert(key, value);
        Ok(())
    }

    async fn apply_writes(&self, ops: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_calls.fetch_add(1, Ordering::SeqCst);
        let mut data = self.data.lock().unwrap();
        for op in ops {
            match op {
                AdapterWriteOp::Put { key, value } => {
                    data.insert(key, value);
                }
                AdapterWriteOp::Delete { key } => {
                    data.remove(&key);
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().remove(&key).is_some())
    }

    async fn exists(&self, key: Vec<u8>) -> Result<bool> {
        Ok(self.data.lock().unwrap().contains_key(&key))
    }

    async fn write_batch(&self, batch: Vec<AdapterWriteOp>) -> Result<()> {
        self.apply_writes(batch).await
    }

    async fn delete_range(&self, start: Vec<u8>, end: Vec<u8>) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        let doomed: Vec<Vec<u8>> = data.range(start..end).map(|(k, _)| k.clone()).collect();
        for key in doomed {
            data.remove(&key);
        }
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.data.lock().unwrap().len())
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.data.lock().unwrap().is_empty())
    }

    async fn clear(&self) -> Result<()> {
        self.data.lock().unwrap().clear();
        Ok(())
    }

    async fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .range(prefix.to_vec()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect())
    }
}
```

`rename_subkey.rs` 改为 `use super::map_adapter::MapAdapter;`, 删除 `contains_separator` 及测试 `hset_rejects_a_parent_key_containing_the_separator` (该行为在 Task 6 反转并有新测试). `mod.rs` 加 `pub mod map_adapter;` 与 `pub mod collection_lifecycle;`.

- [ ] **Step 2: 写失败的存储层测试 (`collection_lifecycle.rs`)**

```rust
//! 大集合生命周期: 版本化 subkey 的存储层与命令层回归测试.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use aikv::storage::adapter::{KvStorageAdapter, StorageAdapter};
use aikv::storage::collection::{CollectionWrite, HeaderWrite};
use aikv::storage::subkey::subkey_db_prefix;
use aikv::storage::{CollectionKind, KvStorage, StoredValue, ValueType};

use super::map_adapter::MapAdapter;

fn adapter() -> (Arc<MapAdapter>, Arc<dyn KvStorage>) {
    let map = Arc::new(MapAdapter::new());
    let storage: Arc<dyn KvStorage> =
        KvStorageAdapter::new(Arc::clone(&map) as Arc<dyn StorageAdapter>);
    (map, storage)
}

fn big_hash(n: usize) -> HashMap<Vec<u8>, Vec<u8>> {
    (0..n)
        .map(|i| (format!("f{i}").into_bytes(), format!("v{i}").into_bytes()))
        .collect()
}

fn header_of(stored: &StoredValue) -> (CollectionKind, u64, u64) {
    match stored.value {
        ValueType::CollectionHeader { kind, count, version } => (kind, count, version),
        ref other => panic!("expected header, got {other:?}"),
    }
}

#[tokio::test]
async fn put_value_converts_large_hash_and_expands_back() {
    let (_map, storage) = adapter();
    let hash = big_hash(100);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(hash.clone()), expires_at: None })
        .await
        .unwrap();
    let raw = storage.get_typed(0, b"h").await.unwrap().unwrap();
    let (kind, count, _) = header_of(&raw);
    assert_eq!((kind, count), (CollectionKind::Hash, 100));
    let expanded = storage.get_expanded(0, b"h").await.unwrap().unwrap();
    assert_eq!(expanded.value, ValueType::Hash(hash));
}

#[tokio::test]
async fn put_value_keeps_small_hash_inline() {
    let (_map, storage) = adapter();
    let hash = big_hash(64);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(hash.clone()), expires_at: None })
        .await
        .unwrap();
    let raw = storage.get_typed(0, b"h").await.unwrap().unwrap();
    assert_eq!(raw.value, ValueType::Hash(hash));
}

#[tokio::test]
async fn commit_collection_writes_in_one_call() {
    let (map, storage) = adapter();
    storage
        .put_value(0, b"s", StoredValue::new_set((0..70).map(|i| vec![i]).collect::<HashSet<_>>()))
        .await
        .unwrap();
    let (kind, count, version) = header_of(&storage.get_typed(0, b"s").await.unwrap().unwrap());
    let before = map.apply_calls();
    storage
        .commit_collection(
            0,
            b"s",
            CollectionWrite {
                header: HeaderWrite::Put(StoredValue {
                    value: ValueType::CollectionHeader { kind, count: count + 2, version },
                    expires_at: None,
                }),
                version,
                kind,
                puts: vec![(b"x".to_vec(), Vec::new()), (b"y".to_vec(), Vec::new())],
                deletes: vec![vec![0]],
            },
        )
        .await
        .unwrap();
    assert_eq!(map.apply_calls() - before, 1);
    let got = storage
        .subkey_get_many(0, b"s", version, kind, &[b"x", &[0u8]])
        .await
        .unwrap();
    assert_eq!(got, vec![Some(Vec::new()), None]);
}

#[tokio::test]
async fn flushdb_clears_subkey_space() {
    let (map, storage) = adapter();
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: None })
        .await
        .unwrap();
    storage.clear(0).await.unwrap();
    assert!(map.keys_with_prefix(&subkey_db_prefix(0)).is_empty());
}
```

(`CollectionKind` / `StoredValue` / `ValueType` / `KvStorage` 均已在 `aikv::storage` 根导出.)

- [ ] **Step 3: 确认失败**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1`
Expected: 编译失败, `put_value` / `CollectionWrite` 等未定义.

- [ ] **Step 4: types.rs 改 header 与 trait**

```rust
    CollectionHeader {
        kind: CollectionKind,
        count: u64,
        version: u64,
    },
```

`KvStorage` trait 删除 `raw_subkey_get/set/delete/for_each` 四个默认方法 (Task 9 前 hash.rs/set.rs 仍引用它们, 所以本任务先**保留**这四个方法, 只新增; Task 9 删除). 新增:

```rust
    /// 是否支持 subkey 布局. 内存引擎不支持, 大集合一律内联.
    fn supports_subkeys(&self) -> bool {
        false
    }

    async fn subkey_get_many(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        subs: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        let _ = (db, key, version, kind, subs);
        Err(subkey_unsupported())
    }

    /// 返回 (sub, value), 按 sub 字节序.
    async fn subkey_scan(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let _ = (db, key, version, kind);
        Err(subkey_unsupported())
    }

    /// subkey 增删与 header 改写一次提交.
    async fn commit_collection(&self, db: usize, key: &[u8], write: CollectionWrite) -> Result<()> {
        let _ = (db, key, write);
        Err(subkey_unsupported())
    }

    /// 写入完整值. 支持 subkey 的引擎会把超过阈值的 Hash/Set 转为新版本 subkey 布局.
    async fn put_value(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        self.set_typed(db, key, value).await
    }

    /// 读取完整值; subkey 布局展开为内联 Hash/Set.
    async fn get_expanded(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.get_typed(db, key).await
    }
```

`rewrite_stored` 默认实现中 `self.set_typed(db, key, stored).await?;` 改为 `self.put_value(db, key, stored).await?;`.

文件内新增:

```rust
fn subkey_unsupported() -> Error {
    Error::Storage("subkey layout is not supported by this engine".into())
}
```

`use crate::storage::collection::CollectionWrite;`

- [ ] **Step 5: collection.rs 追加写入描述**

```rust
use crate::storage::types::{CollectionKind, StoredValue};

/// header 的写法.
pub enum HeaderWrite {
    /// 改写已存在的 header; TTL 计数净变化为 0 (同 `rewrite_stored`).
    Put(StoredValue),
    Delete { had_ttl: bool },
}

/// 对同一集合同一版本的一次提交.
pub struct CollectionWrite {
    pub header: HeaderWrite,
    pub version: u64,
    pub kind: CollectionKind,
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}
```

- [ ] **Step 6: 新建 `src/storage/adapter/collection_ops.rs`**

`adapter.rs` 顶部 (use 之后) 加 `mod collection_ops;`. 文件内容:

```rust
//! `KvStorageAdapter` 的集合 (subkey 布局) 读写.

use crate::error::Result;
use crate::storage::adapter::{AdapterWriteOp, KvStorageAdapter};
use crate::storage::collection::{
    next_collection_version, CollectionWrite, HeaderWrite, HASH_MAX_INLINE_FIELDS,
    SET_MAX_INLINE_MEMBERS,
};
use crate::storage::subkey::{encode_subkey, subkey_version_prefix};
use crate::storage::types::{CollectionKind, StoredValue, ValueType};

/// 暂存写每批条数; 远低于集群批处理器的 512 条.
const STAGE_CHUNK: usize = 256;

impl KvStorageAdapter {
    pub(super) async fn subkey_get_many_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        subs: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.check_db(db)?;
        let mut out = Vec::with_capacity(subs.len());
        for sub in subs {
            out.push(self.storage.get(encode_subkey(db, key, version, kind, sub)).await?);
        }
        Ok(out)
    }

    pub(super) async fn subkey_scan_impl(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.check_db(db)?;
        let prefix = subkey_version_prefix(db, key, version, kind);
        let skip = prefix.len();
        let mut rows = self.storage.scan_prefix(&prefix).await?;
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows
            .into_iter()
            .map(|(physical, value)| (physical[skip..].to_vec(), value))
            .collect())
    }

    pub(super) async fn commit_collection_impl(
        &self,
        db: usize,
        key: &[u8],
        write: CollectionWrite,
    ) -> Result<()> {
        self.check_db(db)?;
        let CollectionWrite { header, version, kind, puts, deletes } = write;
        let mut ops = Vec::with_capacity(puts.len() + deletes.len() + 2);
        for (sub, value) in puts {
            ops.push(AdapterWriteOp::Put { key: encode_subkey(db, key, version, kind, &sub), value });
        }
        for sub in deletes {
            ops.push(AdapterWriteOp::Delete { key: encode_subkey(db, key, version, kind, &sub) });
        }
        let cleared_ttl = match header {
            HeaderWrite::Put(stored) => {
                ops.push(AdapterWriteOp::Put {
                    key: Self::encode(db, key),
                    value: Self::serialize(&stored)?,
                });
                false
            }
            HeaderWrite::Delete { had_ttl } => {
                ops.push(AdapterWriteOp::Delete { key: Self::encode(db, key) });
                had_ttl
            }
        };
        if let Some((meta_key, meta)) = self.next_watch_meta_put(db, key).await? {
            ops.push(AdapterWriteOp::Put { key: meta_key, value: meta });
        }
        self.storage.apply_writes(ops).await?;
        if cleared_ttl {
            self.expires.on_ttl_clear(db);
        }
        Ok(())
    }

    /// 新版本 subkey 分批写入; 调用方随后再写 header.
    pub(super) async fn stage_subkeys(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        entries: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<()> {
        for chunk in entries.chunks(STAGE_CHUNK) {
            let ops = chunk
                .iter()
                .map(|(sub, value)| AdapterWriteOp::Put {
                    key: encode_subkey(db, key, version, kind, sub),
                    value: value.clone(),
                })
                .collect();
            self.storage.apply_writes(ops).await?;
        }
        Ok(())
    }

    /// 超过阈值的 Hash/Set 转为新版本 header, 其余原样返回.
    pub(super) async fn stage_if_large(
        &self,
        db: usize,
        key: &[u8],
        value: StoredValue,
    ) -> Result<StoredValue> {
        let StoredValue { value, expires_at } = value;
        let (kind, entries): (CollectionKind, Vec<(Vec<u8>, Vec<u8>)>) = match value {
            ValueType::Hash(map) if map.len() > HASH_MAX_INLINE_FIELDS => {
                (CollectionKind::Hash, map.into_iter().collect())
            }
            ValueType::Set(set) if set.len() > SET_MAX_INLINE_MEMBERS => (
                CollectionKind::Set,
                set.into_iter().map(|member| (member, Vec::new())).collect(),
            ),
            other => return Ok(StoredValue { value: other, expires_at }),
        };
        let version = next_collection_version();
        let count = entries.len() as u64;
        self.stage_subkeys(db, key, version, kind, entries).await?;
        Ok(StoredValue {
            value: ValueType::CollectionHeader { kind, count, version },
            expires_at,
        })
    }

    pub(super) async fn put_value_impl(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        self.check_db(db)?;
        let value = self.stage_if_large(db, key, value).await?;
        self.set_typed_raw(db, key, value).await
    }

    pub(super) async fn get_expanded_impl(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        let Some(stored) = self.load_typed(db, key).await? else {
            return Ok(None);
        };
        let ValueType::CollectionHeader { kind, version, .. } = stored.value else {
            return Ok(Some(stored));
        };
        let rows = self.subkey_scan_impl(db, key, version, kind).await?;
        let value = match kind {
            CollectionKind::Hash => ValueType::Hash(rows.into_iter().collect()),
            CollectionKind::Set => ValueType::Set(rows.into_iter().map(|(m, _)| m).collect()),
        };
        Ok(Some(StoredValue { value, expires_at: stored.expires_at }))
    }
}
```

- [ ] **Step 7: adapter.rs 委托与 `clear_db`**

- 现有 `KvStorage::set_typed` 的函数体移到私有 `pub(super) async fn set_typed_raw(&self, db, key, value)` (在 `impl KvStorageAdapter` 中), `set_typed` 改为调用它. 字段与 `check_db`/`encode`/`serialize`/`load_typed`/`next_watch_meta_put` 对子模块可见 (子模块可访问父模块私有项), 无需改可见性.
- `impl KvStorage for KvStorageAdapter` 追加:

```rust
    fn supports_subkeys(&self) -> bool {
        true
    }

    async fn subkey_get_many(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
        subs: &[&[u8]],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        self.subkey_get_many_impl(db, key, version, kind, subs).await
    }

    async fn subkey_scan(
        &self,
        db: usize,
        key: &[u8],
        version: u64,
        kind: CollectionKind,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.subkey_scan_impl(db, key, version, kind).await
    }

    async fn commit_collection(&self, db: usize, key: &[u8], write: CollectionWrite) -> Result<()> {
        self.commit_collection_impl(db, key, write).await
    }

    async fn put_value(&self, db: usize, key: &[u8], value: StoredValue) -> Result<()> {
        self.put_value_impl(db, key, value).await
    }

    async fn get_expanded(&self, db: usize, key: &[u8]) -> Result<Option<StoredValue>> {
        self.get_expanded_impl(db, key).await
    }
```

- `clear_db` 在删主键区间之后追加:

```rust
        let sub_prefix = crate::storage::subkey::subkey_db_prefix(db);
        let sub_end = AiDbEngine::prefix_end(&sub_prefix).expect("subkey prefix ends with 0x01");
        self.storage.delete_range(sub_prefix, sub_end).await?;
```

- 模块文档 "Key 布局" 的 subkey 行改为 `subkey:  {db_index}\x01{len u32}{user_key}{version u64}{H|S}{sub}  # 大 Hash/Set, 独立键空间`.

- [ ] **Step 8: hash.rs / set.rs 编译修补 (不改行为)**

- 所有 `ValueType::CollectionHeader { kind: ..., count: X }` 构造处补 `version`: 在 `match` 已绑定 header 的分支里用 `version` 字段原值 (模式加 `version`), 在 `migrate_*_to_subkey` 中写 `version: 0`.
- `count` 运算中的 `as u32` 改为 `as u64`.
- 这些代码在 Task 6/7 整体替换, 此处只求编译.

- [ ] **Step 9: 跑测试**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1 && cargo test --test storage rename_subkey -- --test-threads=1`
Expected: collection_lifecycle 4 个 PASS; rename_subkey 中依赖 HSET 的两个用例此时可能失败 (旧写入路径 + 新 transfer 前), 若失败记录下来, 由 Task 5 修复并在 Task 5 Step 末尾复跑.

- [ ] **Step 10: 提交 (需用户审阅)**

```bash
git add src/storage tests/modules/storage
git commit -m "feat: 集合存储 API 支持版本化 subkey 提交、暂存写与展开"
```

---

### Task 5: 跨 key 操作 (RENAME/COPY/MOVE/SWAPDB/DUMP/RESTORE/OBJECT)

**Files:**
- Modify: `aikv/src/storage/adapter.rs` (删除 `transfer_key` / `collection_subkeys`, 改 `swap_db`, 新增 `move_key` 委托)
- Modify: `aikv/src/storage/adapter/collection_ops.rs` (新 `transfer_key`, `restage_entries`, `clear_main_range`)
- Modify: `aikv/src/storage/types.rs` (`KvStorage::move_key` 默认实现)
- Modify: `aikv/src/command/database.rs:61-80`
- Modify: `aikv/src/command/key.rs:228-286` (DUMP, RESTORE) 与 MIGRATE 取值处
- Modify: `aikv/src/command/router/mod.rs:664`
- Test: `aikv/tests/modules/storage/collection_lifecycle.rs`, `rename_subkey.rs`

**Interfaces:**
- Consumes: Task 4 的 `stage_subkeys`, `stage_if_large`, `subkey_scan_impl`, `set_typed_raw`.
- Produces: `KvStorage::move_key(&self, src_db: usize, dst_db: usize, key: &[u8]) -> Result<bool>`

- [ ] **Step 1: 写失败的回归测试 (追加到 `collection_lifecycle.rs`)**

```rust
async fn put_big_hash(storage: &Arc<dyn KvStorage>, db: usize, key: &[u8], n: usize) {
    storage
        .put_value(db, key, StoredValue { value: ValueType::Hash(big_hash(n)), expires_at: None })
        .await
        .unwrap();
}

/// MOVE 大 Hash 后目标库只有 header, field 全部丢失.
/// 期望: 目标库可读出全部 field, 源库不存在. Issue #P3
#[tokio::test]
async fn move_large_hash_keeps_fields() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    assert!(storage.move_key(0, 1, b"h").await.unwrap());
    assert!(storage.get_typed(0, b"h").await.unwrap().is_none());
    let moved = storage.get_expanded(1, b"h").await.unwrap().unwrap();
    assert_eq!(moved.value, ValueType::Hash(big_hash(100)));
}

/// SWAPDB 只搬 header, 大集合数据丢失.
/// 期望: 交换后两边的大集合内容完整. Issue #P4
#[tokio::test]
async fn swapdb_keeps_large_collections() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"a", 80).await;
    put_big_hash(&storage, 1, b"b", 90).await;
    storage.swap_db(0, 1).await.unwrap();
    let a = storage.get_expanded(1, b"a").await.unwrap().unwrap();
    let b = storage.get_expanded(0, b"b").await.unwrap().unwrap();
    assert_eq!(a.value, ValueType::Hash(big_hash(80)));
    assert_eq!(b.value, ValueType::Hash(big_hash(90)));
    assert!(storage.get_typed(0, b"a").await.unwrap().is_none());
}

/// RENAME 覆盖已有大 Hash 时, 目标旧 field 不得混入.
/// 期望: 目标内容等于源内容. Issue #P1
#[tokio::test]
async fn rename_over_existing_large_hash_has_no_stale_fields() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"src", 70).await;
    let mut other = big_hash(0);
    for i in 0..100 {
        other.insert(format!("old{i}").into_bytes(), b"x".to_vec());
    }
    storage
        .put_value(0, b"dst", StoredValue { value: ValueType::Hash(other), expires_at: None })
        .await
        .unwrap();
    storage.rename_key(0, b"src", b"dst").await.unwrap();
    let dst = storage.get_expanded(0, b"dst").await.unwrap().unwrap();
    assert_eq!(dst.value, ValueType::Hash(big_hash(70)));
}
```

命令层 DUMP/RESTORE 与 OBJECT ENCODING (同文件):

```rust
use aikv::command::CommandRouter;
use aikv::protocol::RespValue;
use bytes::Bytes;

fn hset_args(key: &str, n: usize) -> Vec<Bytes> {
    let mut args = vec![Bytes::copy_from_slice(key.as_bytes())];
    for i in 0..n {
        args.push(Bytes::from(format!("f{i}")));
        args.push(Bytes::from(format!("v{i}")));
    }
    args
}

async fn exec(router: &CommandRouter, cmd: &str, args: &[&[u8]]) -> RespValue {
    let args: Vec<Bytes> = args.iter().map(|a| Bytes::copy_from_slice(a)).collect();
    let mut db = 0usize;
    router.execute(cmd, &args, &mut db).await.unwrap()
}

/// DUMP 大 Hash 只序列化 header, RESTORE 后 field 丢失.
/// 期望: RESTORE 到新 key 后 HGETALL 与原值一致. Issue #P5
#[tokio::test]
async fn dump_restore_large_hash_roundtrip() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    let router = CommandRouter::new(storage.clone());
    let RespValue::BulkString(Some(payload)) = exec(&router, "DUMP", &[b"h"]).await else {
        panic!("dump");
    };
    exec(&router, "RESTORE", &[b"h2", b"0", &payload]).await;
    let restored = storage.get_expanded(0, b"h2").await.unwrap().unwrap();
    assert_eq!(restored.value, ValueType::Hash(big_hash(100)));
}

#[tokio::test]
async fn object_encoding_reports_hashtable_for_subkey_layout() {
    let (_map, storage) = adapter();
    put_big_hash(&storage, 0, b"h", 100).await;
    let router = CommandRouter::new(storage);
    assert_eq!(
        exec(&router, "OBJECT", &[b"ENCODING", b"h"]).await,
        RespValue::BulkString(Some(Bytes::from_static(b"hashtable")))
    );
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1`
Expected: `move_large_hash_keeps_fields` 编译失败 (`move_key` 未定义); 补上 trait 签名后 MOVE/SWAPDB/RENAME/DUMP/OBJECT 用例 FAIL.

- [ ] **Step 3: `KvStorage::move_key` 默认实现 (types.rs)**

```rust
    /// MOVE: 目标库已存在同名 key 时返回 false.
    async fn move_key(&self, src_db: usize, dst_db: usize, key: &[u8]) -> Result<bool> {
        let Some(value) = self.get_typed(src_db, key).await? else {
            return Ok(false);
        };
        if self.get_typed(dst_db, key).await?.is_some() {
            return Ok(false);
        }
        self.set_typed(dst_db, key, value).await?;
        self.delete(src_db, key).await?;
        Ok(true)
    }
```

- [ ] **Step 4: collection_ops.rs 新 transfer 与 SWAPDB 辅助**

```rust
impl KvStorageAdapter {
    /// subkey 布局的值复制为目标 key 的新版本 (暂存写), 返回目标 header; 其它值原样返回.
    async fn restage(
        &self,
        src_db: usize,
        src_key: &[u8],
        dst_db: usize,
        dst_key: &[u8],
        value: StoredValue,
    ) -> Result<StoredValue> {
        let ValueType::CollectionHeader { kind, count, version } = value.value else {
            return Ok(value);
        };
        let rows = self.subkey_scan_impl(src_db, src_key, version, kind).await?;
        let new_version = next_collection_version();
        self.stage_subkeys(dst_db, dst_key, new_version, kind, rows).await?;
        Ok(StoredValue {
            value: ValueType::CollectionHeader { kind, count, version: new_version },
            expires_at: value.expires_at,
        })
    }

    /// RENAME / COPY / MOVE. 目标原有 subkey 与源旧版本 subkey 都交给 GC.
    pub(super) async fn transfer_key(
        &self,
        src_db: usize,
        dst_db: usize,
        old_key: &[u8],
        new_key: &[u8],
        value: StoredValue,
        delete_source: bool,
    ) -> Result<()> {
        let value = self.restage(src_db, old_key, dst_db, new_key, value).await?;
        let had_ttl = value.expires_at.is_some();
        let mut ops = vec![AdapterWriteOp::Put {
            key: Self::encode(dst_db, new_key),
            value: Self::serialize(&value)?,
        }];
        if let Some((key, meta)) = self.next_watch_meta_put(dst_db, new_key).await? {
            ops.push(AdapterWriteOp::Put { key, value: meta });
        }
        if delete_source {
            ops.push(AdapterWriteOp::Delete { key: Self::encode(src_db, old_key) });
            if let Some((key, meta)) = self.next_watch_meta_put(src_db, old_key).await? {
                ops.push(AdapterWriteOp::Put { key, value: meta });
            }
        }
        self.storage.apply_writes(ops).await?;
        if had_ttl {
            self.expires.on_ttl_write(dst_db);
            if delete_source {
                self.expires.on_ttl_clear(src_db);
            }
        }
        Ok(())
    }

    pub(super) async fn move_key_impl(&self, src_db: usize, dst_db: usize, key: &[u8]) -> Result<bool> {
        let Some(value) = self.load_typed(src_db, key).await? else {
            return Ok(false);
        };
        if self.load_typed(dst_db, key).await?.is_some() {
            return Ok(false);
        }
        self.transfer_key(src_db, dst_db, key, key, value, true).await?;
        Ok(true)
    }

    /// SWAPDB: 先把两边的大集合以新版本暂存到对侧, 再清主键区间并写回.
    /// 清区间与写回之间不原子, 与原实现一致.
    pub(super) async fn swap_db_impl(&self, a: usize, b: usize) -> Result<()> {
        let entries_a = self.collect_db_entries(a).await?;
        let entries_b = self.collect_db_entries(b).await?;
        let mut into_b = Vec::with_capacity(entries_a.len());
        for (key, value) in entries_a {
            into_b.push((key.clone(), self.restage(a, &key, b, &key, value).await?));
        }
        let mut into_a = Vec::with_capacity(entries_b.len());
        for (key, value) in entries_b {
            into_a.push((key.clone(), self.restage(b, &key, a, &key, value).await?));
        }
        self.clear_main_range(a).await?;
        self.clear_main_range(b).await?;
        for (key, value) in into_a {
            self.set_typed_raw(a, &key, value).await?;
        }
        for (key, value) in into_b {
            self.set_typed_raw(b, &key, value).await?;
        }
        Ok(())
    }
}
```

`adapter.rs`:
- 删除旧 `transfer_key` 与 `collection_subkeys` (约 438-533 行).
- 把 `clear_db` 拆成 `pub(super) async fn clear_main_range(&self, db)` (原主键区间删除 + `self.expires.reset_db(db)`) 与 `clear_db` (= `clear_main_range` + Task 4 的 subkey 区间删除).
- `swap_db` 保留 `check_db` 与 `a == b` 判断, 其余改为 `self.swap_db_impl(a, b).await`.
- `impl KvStorage` 追加 `async fn move_key(&self, src_db, dst_db, key) -> Result<bool> { self.check_db(src_db)?; self.check_db(dst_db)?; self.move_key_impl(src_db, dst_db, key).await }`.
- `CollectionKind` 若不再使用则从 import 删除.

- [ ] **Step 5: 命令层**

`database.rs` MOVE 中

```rust
        let Some(value) = self.storage.get_typed(src_db, key).await? else {
            return Ok(router::integer(0));
        };
        if self.storage.get_typed(target, key).await?.is_some() {
            return Ok(router::integer(0));
        }
        self.storage.set_typed(target, key, value).await?;
        self.storage.delete(src_db, key).await?;
        Ok(router::integer(1))
```

改为

```rust
        let moved = self.storage.move_key(src_db, target, key).await?;
        Ok(router::integer(i64::from(moved)))
```

`key.rs`:
- DUMP: `self.storage.get_typed(db, &args[0])` → `self.storage.get_expanded(db, &args[0])`.
- RESTORE: `self.storage.set_typed(db, key, stored)` → `self.storage.put_value(db, key, stored)`; 日志文案 `set_typed failed` → `put_value failed`.
- MIGRATE: `key.rs:348` 的 `self.storage.get_typed(db, key)` 改为 `get_expanded`; 文件头注释 `get_typed → dump_encode` 同步改为 `get_expanded → dump_encode`.

`router/mod.rs:664`: `ValueType::CollectionHeader { .. } => "listpack",` 改为 `ValueType::CollectionHeader { .. } => "hashtable",`.

- [ ] **Step 6: 跑测试**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1 && cargo test --test storage rename_subkey -- --test-threads=1 && cargo test --test commands -- key:: database:: --test-threads=1`
Expected: PASS. (`rename_subkey` 的 HSET 用例依赖 Task 6 的 HSET 才走新格式; 若此时失败, 在 Task 6 Step 末尾复跑并必须通过.)

- [ ] **Step 7: 提交 (需用户审阅)**

```bash
git add src/storage src/command/database.rs src/command/key.rs src/command/router/mod.rs tests/modules/storage
git commit -m "fix: MOVE/SWAPDB/DUMP/RESTORE/RENAME 正确处理大集合

Fixes #P3
Fixes #P4
Fixes #P5"
```

---

### Task 6: Hash 命令改用集合 API

**Files:**
- Modify: `aikv/src/command/hash.rs` (重写 subkey 分支与 helper)
- Modify: `aikv/src/command/scan_util.rs:46-51`
- Test: `aikv/tests/modules/storage/collection_lifecycle.rs`

**Interfaces:**
- Consumes: `KvStorage::{supports_subkeys, subkey_get_many, subkey_scan, commit_collection, rewrite_stored}`, `HeaderWrite`, `CollectionWrite`.
- Produces: hash.rs 内部 `enum HashState { Missing, Inline(StoredValue), Subkey { expires_at: Option<u64>, count: u64, version: u64 } }` 与 helper `load_state`, `write_fields`, `read_fields`, `load_all`.

- [ ] **Step 1: 写失败的回归测试 (双引擎)**

```rust
use aikv::storage::MemoryEngine;

fn engines() -> Vec<(&'static str, Arc<dyn KvStorage>)> {
    let (_map, adapter_storage) = adapter();
    vec![("memory", MemoryEngine::new(16) as Arc<dyn KvStorage>), ("adapter", adapter_storage)]
}

async fn run(router: &CommandRouter, cmd: &str, args: Vec<Bytes>) -> RespValue {
    let mut db = 0usize;
    router.execute(cmd, &args, &mut db).await.unwrap()
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

/// 内存引擎第 65 个 field 报错 (不支持 raw subkey).
/// 期望: 两种引擎都能写入 100 个 field 且 HLEN=100. Issue #P7
#[tokio::test]
async fn hset_beyond_threshold_works_on_both_engines() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        assert_eq!(run(&router, "HSET", hset_args("h", 100)).await, RespValue::Integer(100), "{name}");
        assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(100), "{name}");
        assert_eq!(
            run(&router, "HGET", vec![b("h"), b("f99")]).await,
            RespValue::BulkString(Some(b("v99"))),
            "{name}"
        );
    }
}

/// DEL 只删 header, 重建同名大 Hash 后旧 field 复活.
/// 期望: 重建后只有新 field. Issue #P1
#[tokio::test]
async fn del_then_recreate_large_hash_has_no_stale_fields() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        run(&router, "DEL", vec![b("h")]).await;
        let mut args = vec![b("h")];
        for i in 0..70 {
            args.push(Bytes::from(format!("n{i}")));
            args.push(b("x"));
        }
        run(&router, "HSET", args).await;
        assert_eq!(run(&router, "HGET", vec![b("h"), b("f0")]).await, RespValue::BulkString(None), "{name}");
        assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(70), "{name}");
    }
}

/// 已过期的大 Hash 被同名重建后旧 field 复活.
/// 期望: 过期后重建只看到新 field. Issue #P2
#[tokio::test]
async fn expired_large_hash_does_not_resurrect() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        run(&router, "PEXPIRE", vec![b("h"), b("1")]).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        run(&router, "HSET", vec![b("h"), b("only"), b("1")]).await;
        assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(1), "{name}");
    }
}

/// 多 field HSET 逐 field 读写, 一次命令提交 N 次.
/// 期望: 对已是 subkey 布局的 Hash, 一条 HSET 只调用一次 apply_writes. Issue #P9
#[tokio::test]
async fn multi_field_hset_commits_once() {
    let (map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "HSET", hset_args("h", 100)).await;
    let before = map.apply_calls();
    let mut args = vec![b("h")];
    for i in 0..50 {
        args.push(Bytes::from(format!("g{i}")));
        args.push(b("v"));
    }
    assert_eq!(run(&router, "HSET", args).await, RespValue::Integer(50));
    assert_eq!(map.apply_calls() - before, 1);
}

#[tokio::test]
async fn hset_counts_duplicate_fields_once() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let got = run(&router, "HSET", vec![b("h"), b("dup"), b("1"), b("dup"), b("2")]).await;
        assert_eq!(got, RespValue::Integer(1), "{name}");
        assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(101), "{name}");
        assert_eq!(run(&router, "HGET", vec![b("h"), b("dup")]).await, RespValue::BulkString(Some(b("2"))), "{name}");
    }
}

/// 父 key 含 0x01 被拒绝.
/// 期望: 任意字节的 key 都能作为大 Hash 使用. Issue #P12
#[tokio::test]
async fn hash_key_with_separator_byte_is_allowed() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    let mut args = hset_args("x", 100);
    args[0] = Bytes::from_static(b"a\x01b");
    assert_eq!(run(&router, "HSET", args).await, RespValue::Integer(100));
    assert_eq!(
        run(&router, "HGET", vec![Bytes::from_static(b"a\x01b"), b("f1")]).await,
        RespValue::BulkString(Some(b("v1")))
    );
}

/// HSCAN 游标超过元素数时切片越界 panic.
/// 期望: 返回游标 0 与空页. Issue #P11
#[tokio::test]
async fn hscan_cursor_past_end_returns_empty_page() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let got = run(&router, "HSCAN", vec![b("h"), b("1000")]).await;
        assert_eq!(
            got,
            RespValue::Array(Some(vec![RespValue::BulkString(Some(b("0"))), RespValue::Array(Some(vec![]))])),
            "{name}"
        );
    }
}

#[tokio::test]
async fn hdel_to_empty_removes_large_hash() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        let mut args = vec![b("h")];
        for i in 0..100 {
            args.push(Bytes::from(format!("f{i}")));
        }
        assert_eq!(run(&router, "HDEL", args).await, RespValue::Integer(100), "{name}");
        assert_eq!(run(&router, "EXISTS", vec![b("h")]).await, RespValue::Integer(0), "{name}");
    }
}

#[tokio::test]
async fn hincrby_and_hsetnx_on_large_hash() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        run(&router, "HSET", hset_args("h", 100)).await;
        assert_eq!(run(&router, "HINCRBY", vec![b("h"), b("n"), b("5")]).await, RespValue::Integer(5), "{name}");
        assert_eq!(run(&router, "HSETNX", vec![b("h"), b("n"), b("9")]).await, RespValue::Integer(0), "{name}");
        assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(101), "{name}");
    }
}
```

`#P10` (读错误被吞) 无法用 `MapAdapter` 触发; 用代码审查保证 hash.rs/set.rs 不再出现 `unwrap_or_default()` 包裹存储调用 (Step 5 中 `rg` 校验).

- [ ] **Step 2: 确认失败**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1`
Expected: `hset_beyond_threshold_works_on_both_engines` (memory)、`del_then_recreate...`、`expired...`、`multi_field_hset_commits_once`、`hash_key_with_separator_byte_is_allowed`、`hscan_cursor_past_end...` FAIL.

- [ ] **Step 3: scan_util 修越界**

```rust
pub(crate) fn paginate_slice<T>(entries: &[T], cursor: u64, count: usize) -> (u64, &[T]) {
    let skip = (cursor as usize).min(entries.len());
    let end = skip.saturating_add(count).min(entries.len());
    let next_cursor = if end < entries.len() { end as u64 } else { 0 };
    (next_cursor, &entries[skip..end])
}
```

- [ ] **Step 4: 重写 hash.rs 的状态与 helper**

删除: `reject_subkey_parent` / `reject_subkey_field` 调用, `scan_hash_subkeys`, `migrate_hash_to_subkey`, `delete_all_hash_subkeys`, `load_hash` (dead code), `load_or_create_hash`, 文件内 `HASH_MAX_INLINE_FIELDS` 常量 (改用 `crate::storage::collection::HASH_MAX_INLINE_FIELDS` 若仍需要; 转换已由 `rewrite_stored` → `put_value` 完成, 通常不再需要). 新增:

```rust
enum HashState {
    Missing,
    Inline(StoredValue),
    Subkey { expires_at: Option<u64>, count: u64, version: u64 },
}

impl HashCommands {
    async fn load_state(&self, db: usize, key: &[u8]) -> Result<HashState> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(HashState::Missing);
        };
        match stored.value {
            ValueType::Hash(_) => Ok(HashState::Inline(stored)),
            ValueType::CollectionHeader { kind: CollectionKind::Hash, count, version } => {
                Ok(HashState::Subkey { expires_at: stored.expires_at, count, version })
            }
            _ => Err(router::wrongtype()),
        }
    }

    /// 写入 field (同名后者覆盖前者), 返回新增 field 数. 调用方持有 key 锁.
    async fn write_fields(
        &self,
        db: usize,
        key: &[u8],
        state: HashState,
        updates: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Result<i64> {
        let updates: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = updates.into_iter().collect();
        match state {
            HashState::Missing => {
                let added = updates.len() as i64;
                let map: HashMap<_, _> = updates.into_iter().collect();
                self.storage.rewrite_stored(db, key, StoredValue::new_hash(map)).await?;
                Ok(added)
            }
            HashState::Inline(mut stored) => {
                let ValueType::Hash(map) = &mut stored.value else { unreachable!() };
                let mut added = 0i64;
                for (field, value) in updates {
                    if map.insert(field, value).is_none() {
                        added += 1;
                    }
                }
                self.storage.rewrite_stored(db, key, stored).await?;
                Ok(added)
            }
            HashState::Subkey { expires_at, count, version } => {
                let fields: Vec<&[u8]> = updates.keys().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, CollectionKind::Hash, &fields)
                    .await?;
                let added = existing.iter().filter(|v| v.is_none()).count() as u64;
                self.storage
                    .commit_collection(db, key, CollectionWrite {
                        header: HeaderWrite::Put(StoredValue {
                            value: ValueType::CollectionHeader {
                                kind: CollectionKind::Hash,
                                count: count + added,
                                version,
                            },
                            expires_at,
                        }),
                        version,
                        kind: CollectionKind::Hash,
                        puts: updates.into_iter().collect(),
                        deletes: Vec::new(),
                    })
                    .await?;
                Ok(added as i64)
            }
        }
    }

    /// 按顺序读取 field; 不存在为 None.
    async fn read_fields(&self, db: usize, key: &[u8], fields: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(vec![None; fields.len()]),
            HashState::Inline(stored) => {
                let ValueType::Hash(map) = stored.value else { unreachable!() };
                Ok(fields.iter().map(|f| map.get(*f).cloned()).collect())
            }
            HashState::Subkey { version, .. } => {
                self.storage.subkey_get_many(db, key, version, CollectionKind::Hash, fields).await
            }
        }
    }

    async fn load_all(&self, db: usize, key: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(Vec::new()),
            HashState::Inline(stored) => {
                let ValueType::Hash(map) = stored.value else { unreachable!() };
                Ok(map.into_iter().collect())
            }
            HashState::Subkey { version, .. } => {
                self.storage.subkey_scan(db, key, version, CollectionKind::Hash).await
            }
        }
    }
}
```

`StoredValue::new_hash` 若不存在, 用 `StoredValue { value: ValueType::Hash(map), expires_at: None }`.

- [ ] **Step 5: 命令改写对照**

每个命令保留参数校验与 `key_lock`, 主体替换如下:

| 命令 | 新主体 |
|------|--------|
| HSET | `let state = self.load_state(db, key).await?; let n = self.write_fields(db, key, state, pairs).await?; Ok(router::integer(n))`, `pairs` 来自 `args[1..].chunks(2)` |
| HMSET | 不变 (调用 hset) |
| HGET | `let v = self.read_fields(db, key, &[field]).await?.pop().flatten();` 有值 `bulk`, 无值 `nil_bulk` |
| HMGET | `self.read_fields(db, key, &fields)` 逐项映射 |
| HEXISTS | `read_fields(..., &[field])` 判 `is_some()` |
| HLEN | `load_state`: Missing→0, Inline→`map.len()`, Subkey→`count` |
| HKEYS / HVALS / HGETALL | `load_all` |
| HSCAN | `load_all` → 按 pattern 过滤 → 按 field 排序 → `paginate_slice` |
| HDEL | 见下方代码 |
| HSETNX | `let state = self.load_state(..)?; if self.read_fields(db, key, &[field]).await?[0].is_some() { return Ok(integer(0)) }; self.write_fields(db, key, state, vec![(field, value)]).await?; Ok(integer(1))` |
| HINCRBY | 见下方代码 |
| HINCRBYFLOAT | 与 HINCRBY 相同结构, 解析与格式化换成 `parse_f64_field_bytes` / `format_hash_float`, 并保留原有的 `is_finite` 校验 |

HDEL:

```rust
        let _lock = self.key_lock.lock(key).await;
        let fields: std::collections::BTreeSet<&[u8]> = args[1..].iter().map(|f| f.as_ref()).collect();
        let fields: Vec<&[u8]> = fields.into_iter().collect();
        match self.load_state(db, key).await? {
            HashState::Missing => Ok(router::integer(0)),
            HashState::Inline(mut stored) => {
                let ValueType::Hash(map) = &mut stored.value else { unreachable!() };
                let removed = fields.iter().filter(|f| map.remove(**f).is_some()).count() as i64;
                if map.is_empty() {
                    self.storage.delete(db, key).await?;
                } else if removed > 0 {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(router::integer(removed))
            }
            HashState::Subkey { expires_at, count, version } => {
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, CollectionKind::Hash, &fields)
                    .await?;
                let deletes: Vec<Vec<u8>> = fields
                    .iter()
                    .zip(existing)
                    .filter_map(|(f, v)| v.map(|_| f.to_vec()))
                    .collect();
                let removed = deletes.len() as u64;
                if removed == 0 {
                    return Ok(router::integer(0));
                }
                let remaining = count.saturating_sub(removed);
                let header = if remaining == 0 {
                    HeaderWrite::Delete { had_ttl: expires_at.is_some() }
                } else {
                    HeaderWrite::Put(StoredValue {
                        value: ValueType::CollectionHeader { kind: CollectionKind::Hash, count: remaining, version },
                        expires_at,
                    })
                };
                self.storage
                    .commit_collection(db, key, CollectionWrite {
                        header,
                        version,
                        kind: CollectionKind::Hash,
                        puts: Vec::new(),
                        deletes,
                    })
                    .await?;
                Ok(router::integer(removed as i64))
            }
        }
```

(header 为 Delete 时剩余 subkey 无需删除, 由 GC 回收.)

HINCRBY:

```rust
        let _lock = self.key_lock.lock(key).await;
        let state = self.load_state(db, key).await?;
        let current = match &state {
            HashState::Missing => None,
            HashState::Inline(stored) => match &stored.value {
                ValueType::Hash(map) => map.get(&field).cloned(),
                _ => unreachable!(),
            },
            HashState::Subkey { version, .. } => self
                .storage
                .subkey_get_many(db, key, *version, CollectionKind::Hash, &[&field])
                .await?
                .pop()
                .flatten(),
        };
        let new_val = match current {
            None => delta,
            Some(existing) => parse_i64_field(&existing)?.checked_add(delta).ok_or_else(|| {
                Error::Command("ERR increment or decrement would overflow".into())
            })?,
        };
        self.write_fields(db, key, state, vec![(field, new_val.to_string().into_bytes())])
            .await?;
        Ok(router::integer(new_val))
```

校验:

```bash
rg -n "raw_subkey|unwrap_or_default\(\)|encode_hash_field_key|AiDbEngine" src/command/hash.rs
```

Expected: 无输出.

- [ ] **Step 5b: Lua 读路径改用 `get_expanded`**

`src/command/script/transaction.rs` 的 `fetch_stored` 中, overlay 检查之后的整段 (从 `let Some(stored) = storage.get_typed(...)` 到函数末尾的 `match`) 替换为:

```rust
        storage.get_expanded(self.db_index, key).await
```

删除 transaction.rs 中因此不再使用的 import (`subkey`, `AiDbEngine`, `CollectionKind`, `HashSet` 等, 以编译器提示为准). Set 的写入仍是旧格式直到 Task 7, 这段时间 Lua 读大 Set 会得到空集合; Task 7 完成后恢复.

- [ ] **Step 6: 跑测试**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1 && cargo test --test storage rename_subkey -- --test-threads=1 && cargo test --test commands -- hash:: script:: --test-threads=1`
Expected: PASS

- [ ] **Step 7: 提交 (需用户审阅)**

```bash
git add src/command/hash.rs src/command/scan_util.rs src/command/script/transaction.rs tests/modules/storage/collection_lifecycle.rs
git commit -m "fix: Hash 命令改用版本化集合 API, 多 field 写入单次提交

Fixes #P1
Fixes #P2
Fixes #P7
Fixes #P9
Fixes #P10
Fixes #P11
Fixes #P12"
```

---

### Task 7: Set 命令改用集合 API

**Files:**
- Modify: `aikv/src/command/set.rs`
- Test: `aikv/tests/modules/storage/collection_lifecycle.rs`

**Interfaces:**
- Consumes: 同 Task 6.
- Produces: set.rs 内部 `enum SetState { Missing, Inline(StoredValue), Subkey { expires_at: Option<u64>, count: u64, version: u64 } }`, helper `load_state`, `add_members`, `remove_members`, `contains`, `load_members`.

- [ ] **Step 1: 写失败的回归测试**

```rust
fn sadd_args(key: &str, n: usize) -> Vec<Bytes> {
    let mut args = vec![b(key)];
    for i in 0..n {
        args.push(Bytes::from(format!("m{i}")));
    }
    args
}

/// S*STORE 结果超过阈值仍内联存储; 空结果写入空集合.
/// 期望: 大结果为 subkey 布局 (OBJECT ENCODING=hashtable), 空结果删除目标. Issue #P13
#[tokio::test]
async fn sunionstore_respects_threshold_and_empty_result() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "SADD", sadd_args("a", 50)).await;
    let mut more = vec![b("b")];
    for i in 40..100 {
        more.push(Bytes::from(format!("m{i}")));
    }
    run(&router, "SADD", more).await;
    assert_eq!(run(&router, "SUNIONSTORE", vec![b("d"), b("a"), b("b")]).await, RespValue::Integer(100));
    assert_eq!(
        run(&router, "OBJECT", vec![b("ENCODING"), b("d")]).await,
        RespValue::BulkString(Some(b("hashtable")))
    );
    assert_eq!(run(&router, "SINTERSTORE", vec![b("d"), b("a"), b("missing")]).await, RespValue::Integer(0));
    assert_eq!(run(&router, "EXISTS", vec![b("d")]).await, RespValue::Integer(0));
}

/// SMOVE 使目标超过阈值时不转换.
/// 期望: 目标转为 subkey 布局且成员完整. Issue #P13
#[tokio::test]
async fn smove_into_inline_set_converts_past_threshold() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "SADD", sadd_args("dst", 64)).await;
    run(&router, "SADD", vec![b("src"), b("extra")]).await;
    assert_eq!(run(&router, "SMOVE", vec![b("src"), b("dst"), b("extra")]).await, RespValue::Integer(1));
    assert_eq!(run(&router, "SCARD", vec![b("dst")]).await, RespValue::Integer(65));
    assert_eq!(
        run(&router, "OBJECT", vec![b("ENCODING"), b("dst")]).await,
        RespValue::BulkString(Some(b("hashtable")))
    );
}

#[tokio::test]
async fn set_commands_on_large_set_both_engines() {
    for (name, storage) in engines() {
        let router = CommandRouter::new(storage);
        assert_eq!(run(&router, "SADD", sadd_args("s", 100)).await, RespValue::Integer(100), "{name}");
        assert_eq!(run(&router, "SADD", vec![b("s"), b("m1"), b("new"), b("new")]).await, RespValue::Integer(1), "{name}");
        assert_eq!(run(&router, "SISMEMBER", vec![b("s"), b("m5")]).await, RespValue::Integer(1), "{name}");
        assert_eq!(run(&router, "SREM", vec![b("s"), b("m5"), b("m5"), b("nope")]).await, RespValue::Integer(1), "{name}");
        assert_eq!(run(&router, "SCARD", vec![b("s")]).await, RespValue::Integer(100), "{name}");
        let RespValue::Array(Some(popped)) = run(&router, "SPOP", vec![b("s"), b("10")]).await else {
            panic!("{name}: spop");
        };
        assert_eq!(popped.len(), 10, "{name}");
        assert_eq!(run(&router, "SCARD", vec![b("s")]).await, RespValue::Integer(90), "{name}");
        let got = run(&router, "SSCAN", vec![b("s"), b("5000")]).await;
        assert_eq!(
            got,
            RespValue::Array(Some(vec![RespValue::BulkString(Some(b("0"))), RespValue::Array(Some(vec![]))])),
            "{name}"
        );
    }
}
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1`
Expected: 三个新用例 FAIL (memory 引擎第 65 个 member 报错; STORE 未转换; SINTERSTORE 空结果未删除).

- [ ] **Step 3: 重写 set.rs helper**

删除 `reject_subkey_*` 调用、`scan_set_subkeys`、`migrate_set_to_subkey`、`load_or_create_set`, 文件内 `SET_MAX_INLINE_MEMBERS` 常量. 新增:

```rust
enum SetState {
    Missing,
    Inline(StoredValue),
    Subkey { expires_at: Option<u64>, count: u64, version: u64 },
}

impl SetCommands {
    async fn load_state(&self, db: usize, key: &[u8]) -> Result<SetState> {
        let Some(stored) = self.storage.get_typed(db, key).await? else {
            return Ok(SetState::Missing);
        };
        match stored.value {
            ValueType::Set(_) => Ok(SetState::Inline(stored)),
            ValueType::CollectionHeader { kind: CollectionKind::Set, count, version } => {
                Ok(SetState::Subkey { expires_at: stored.expires_at, count, version })
            }
            _ => Err(router::wrongtype()),
        }
    }

    fn header(count: u64, version: u64, expires_at: Option<u64>) -> StoredValue {
        StoredValue {
            value: ValueType::CollectionHeader { kind: CollectionKind::Set, count, version },
            expires_at,
        }
    }

    /// 返回新增成员数. 调用方持有 key 锁.
    async fn add_members(&self, db: usize, key: &[u8], state: SetState, members: Vec<Vec<u8>>) -> Result<i64> {
        let members: std::collections::BTreeSet<Vec<u8>> = members.into_iter().collect();
        match state {
            SetState::Missing => {
                let added = members.len() as i64;
                self.storage
                    .rewrite_stored(db, key, StoredValue::new_set(members.into_iter().collect()))
                    .await?;
                Ok(added)
            }
            SetState::Inline(mut stored) => {
                let ValueType::Set(set) = &mut stored.value else { unreachable!() };
                let added = members.into_iter().filter(|m| set.insert(m.clone())).count() as i64;
                if added > 0 {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(added)
            }
            SetState::Subkey { expires_at, count, version } => {
                let subs: Vec<&[u8]> = members.iter().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, CollectionKind::Set, &subs)
                    .await?;
                let puts: Vec<(Vec<u8>, Vec<u8>)> = members
                    .iter()
                    .zip(existing)
                    .filter(|(_, v)| v.is_none())
                    .map(|(m, _)| (m.clone(), Vec::new()))
                    .collect();
                let added = puts.len() as u64;
                if added == 0 {
                    return Ok(0);
                }
                self.storage
                    .commit_collection(db, key, CollectionWrite {
                        header: HeaderWrite::Put(Self::header(count + added, version, expires_at)),
                        version,
                        kind: CollectionKind::Set,
                        puts,
                        deletes: Vec::new(),
                    })
                    .await?;
                Ok(added as i64)
            }
        }
    }

    /// 返回实际删除的成员. 调用方持有 key 锁.
    async fn remove_members(&self, db: usize, key: &[u8], state: SetState, members: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        let members: std::collections::BTreeSet<Vec<u8>> = members.into_iter().collect();
        match state {
            SetState::Missing => Ok(Vec::new()),
            SetState::Inline(mut stored) => {
                let ValueType::Set(set) = &mut stored.value else { unreachable!() };
                let removed: Vec<Vec<u8>> = members.into_iter().filter(|m| set.remove(m)).collect();
                if set.is_empty() {
                    self.storage.delete(db, key).await?;
                } else if !removed.is_empty() {
                    self.storage.rewrite_stored(db, key, stored).await?;
                }
                Ok(removed)
            }
            SetState::Subkey { expires_at, count, version } => {
                let subs: Vec<&[u8]> = members.iter().map(Vec::as_slice).collect();
                let existing = self
                    .storage
                    .subkey_get_many(db, key, version, CollectionKind::Set, &subs)
                    .await?;
                let removed: Vec<Vec<u8>> = members
                    .into_iter()
                    .zip(existing)
                    .filter_map(|(m, v)| v.map(|_| m))
                    .collect();
                if removed.is_empty() {
                    return Ok(removed);
                }
                let remaining = count.saturating_sub(removed.len() as u64);
                let header = if remaining == 0 {
                    HeaderWrite::Delete { had_ttl: expires_at.is_some() }
                } else {
                    HeaderWrite::Put(Self::header(remaining, version, expires_at))
                };
                self.storage
                    .commit_collection(db, key, CollectionWrite {
                        header,
                        version,
                        kind: CollectionKind::Set,
                        puts: Vec::new(),
                        deletes: removed.clone(),
                    })
                    .await?;
                Ok(removed)
            }
        }
    }

    async fn contains(&self, db: usize, key: &[u8], member: &[u8]) -> Result<bool> {
        match self.load_state(db, key).await? {
            SetState::Missing => Ok(false),
            SetState::Inline(stored) => match stored.value {
                ValueType::Set(set) => Ok(set.contains(member)),
                _ => unreachable!(),
            },
            SetState::Subkey { version, .. } => Ok(self
                .storage
                .subkey_get_many(db, key, version, CollectionKind::Set, &[member])
                .await?
                .pop()
                .flatten()
                .is_some()),
        }
    }

    async fn load_members(&self, db: usize, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        match self.load_state(db, key).await? {
            SetState::Missing => Ok(Vec::new()),
            SetState::Inline(stored) => match stored.value {
                ValueType::Set(set) => Ok(set.into_iter().collect()),
                _ => unreachable!(),
            },
            SetState::Subkey { version, .. } => Ok(self
                .storage
                .subkey_scan(db, key, version, CollectionKind::Set)
                .await?
                .into_iter()
                .map(|(m, _)| m)
                .collect()),
        }
    }
}
```

- [ ] **Step 4: 命令改写对照**

| 命令 | 新主体 |
|------|--------|
| SADD | `let state = self.load_state(db, key).await?; Ok(integer(self.add_members(db, key, state, args[1..].iter().map(|m| m.to_vec()).collect()).await?))` |
| SREM | `remove_members(...)` 返回值的 `len()` |
| SISMEMBER | `contains` |
| SMEMBERS | `load_members` |
| SCARD | `load_state`: Missing→0, Inline→`set.len()`, Subkey→`count` |
| SPOP | `load_state` 后用 `load_members` 取全部 (Missing 返回 `nil_bulk`), 打乱取 n 个, 再用重新 `load_state` 的结果调 `remove_members(picked)`; 返回值格式不变 |
| SRANDMEMBER | `load_members` 替换 `load_set_full` |
| SSCAN | `load_members` → pattern 过滤 → 排序 → `paginate_slice` |
| SMOVE | 见下方代码 |
| compute_union/inter/diff | `load_set_full` 改为 `self.load_members(db, key).await?.into_iter().collect::<HashSet<_>>()` |
| store_set | 见下方代码 |

SPOP 为避免两次 `load_state`, 写成:

```rust
        let state = self.load_state(db, key).await?;
        let mut members = match &state {
            SetState::Missing => return Ok(router::nil_bulk()),
            SetState::Inline(stored) => match &stored.value {
                ValueType::Set(set) => set.iter().cloned().collect::<Vec<_>>(),
                _ => unreachable!(),
            },
            SetState::Subkey { version, .. } => self
                .storage
                .subkey_scan(db, key, *version, CollectionKind::Set)
                .await?
                .into_iter()
                .map(|(m, _)| m)
                .collect(),
        };
        members.shuffle(&mut rand::thread_rng());
        let n = (count as usize).min(members.len());
        members.truncate(n);
        let picked = self.remove_members(db, key, state, members).await?;
```

SMOVE (保留 `lock_two` 与 same_key 判断):

```rust
        if !self.contains(db, source, &member).await? {
            return Ok(router::integer(0));
        }
        let dest_state = self.load_state(db, dest).await?;
        if same_key {
            return Ok(router::integer(1));
        }
        let src_state = self.load_state(db, source).await?;
        self.remove_members(db, source, src_state, vec![member.clone()]).await?;
        self.add_members(db, dest, dest_state, vec![member]).await?;
        Ok(router::integer(1))
```

(`dest_state` 先于删除读取, 保证目标类型错误时源不被修改, 与 Redis 一致.)

store_set:

```rust
    async fn store_set(&self, db: usize, dest: &Bytes, set: HashSet<Vec<u8>>) -> Result<RespValue> {
        let _lock = self.key_lock.lock(dest).await;
        let old = self.storage.get_typed(db, dest).await?;
        let len = set.len() as i64;
        if set.is_empty() {
            if old.is_some() {
                self.storage.delete(db, dest).await?;
            }
            return Ok(router::integer(0));
        }
        self.storage.put_value(db, dest, StoredValue::new_set(set)).await?;
        if old.and_then(|v| v.expires_at).is_some() {
            self.storage.release_seen_ttl(db).await?;
        }
        Ok(router::integer(len))
    }
```

(Redis 的 `*STORE` 覆盖任意类型的目标, 原实现对非 Set 目标返回 WRONGTYPE 是错误行为; 此处按 Redis 语义改为直接覆盖. 若用户希望保持原行为, 恢复类型检查即可.)

校验:

```bash
rg -n "raw_subkey|unwrap_or_default\(\)|encode_set_member_key|AiDbEngine" src/command/set.rs
```

Expected: 无输出.

- [ ] **Step 5: 跑测试**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1 && cargo test --test commands -- set:: --test-threads=1`
Expected: PASS

- [ ] **Step 6: 提交 (需用户审阅)**

```bash
git add src/command/set.rs tests/modules/storage/collection_lifecycle.rs
git commit -m "fix: Set 命令改用版本化集合 API, SMOVE 与 STORE 遵守阈值

Fixes #P13"
```

---

### Task 8: Lua 与 KEYS/SCAN 回归测试

**Files:**
- Modify: `aikv/src/command/script/execute/mod.rs:14` (注释)
- Modify: `aikv/src/storage/adapter.rs` (`keys_for_db` / `collect_db_entries` / `scan` 中 "跳过 subkey entry" 注释)
- Test: `aikv/tests/modules/storage/collection_lifecycle.rs`

- [ ] **Step 1: 写失败的回归测试**

```rust
/// Lua 把大 Hash 展开后内联写回, 之后 HDEL 再让集合转回 subkey 时旧 field 复活.
/// 期望: Lua 删除的 field 之后不会再出现. Issue #P6
#[tokio::test]
async fn lua_rewrite_does_not_resurrect_old_fields() {
    let (_map, storage) = adapter();
    let router = CommandRouter::new(storage);
    run(&router, "HSET", hset_args("h", 100)).await;
    run(
        &router,
        "EVAL",
        vec![b("redis.call('HDEL', KEYS[1], 'f0'); return 1"), b("1"), b("h")],
    )
    .await;
    run(&router, "HSET", vec![b("h"), b("z"), b("1")]).await;
    assert_eq!(run(&router, "HGET", vec![b("h"), b("f0")]).await, RespValue::BulkString(None));
    assert_eq!(run(&router, "HLEN", vec![b("h")]).await, RespValue::Integer(100));
}

/// KEYS/SCAN 靠反序列化失败跳过 subkey; field 值恰好是合法 StoredValue 时被当作 key.
/// 期望: 只返回用户 key. Issue #P8
#[tokio::test]
async fn keys_and_scan_never_return_subkeys() {
    let (_map, storage) = adapter();
    let lookalike = postcard::to_allocvec(&StoredValue::string(b"x".to_vec())).unwrap();
    let mut hash = big_hash(100);
    hash.insert(b"evil".to_vec(), lookalike);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(hash), expires_at: None })
        .await
        .unwrap();
    assert_eq!(storage.keys(0, b"*").await.unwrap(), vec![b"h".to_vec()]);
    let page = storage.scan(0, 0, b"*", 1000).await.unwrap();
    assert_eq!(page.keys, vec![b"h".to_vec()]);
}
```

(`postcard` 若不是 aikv 的 dev-dependency, 它已是普通依赖, 集成测试可直接使用; 若不可用, 用 `aikv::storage::dump` 中已有的序列化函数替代.)

- [ ] **Step 2: 确认结果**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1`
Expected: 两个用例此时可能已经 PASS: P8 由 Task 4 的独立键空间修复, P6 由版本化修复 (Lua 内联写回后旧版本 subkey 不再可见). 这里的作用是锁定回归. 若有失败, 先按 systematic-debugging 定位再继续.

注意: `fetch_stored` 改用 `get_expanded` 已在 Task 6 Step 5b 完成 (否则 Task 6 之后 Lua 读不到新格式 subkey); 本任务 Step 3 只剩注释清理与确认.

- [ ] **Step 3: 注释清理**

确认 `fetch_stored` 已是 Task 6 Step 5b 的形态. `execute/mod.rs:14` 注释改为 "读: txn.get / txn.get_value (写缓冲优先; subkey 布局经 `get_expanded` 展开为完整集合)".

`adapter.rs` 三处 `// 跳过 subkey entry (非 StoredValue 编码)` 改为 `// 主键空间只含 StoredValue; 解码失败的条目跳过`.

- [ ] **Step 4: 跑测试**

Run: `cargo test --test storage collection_lifecycle -- --test-threads=1 && cargo test --test commands -- script:: --test-threads=1 && cargo test --test storage script_commit_batch -- --test-threads=1`
Expected: PASS

- [ ] **Step 5: 提交 (需用户审阅)**

```bash
git add src/command/script src/storage/adapter.rs tests/modules/storage/collection_lifecycle.rs
git commit -m "test: Lua 写回与 KEYS/SCAN 不再暴露旧 subkey 的回归测试

Fixes #P6
Fixes #P8"
```

---

### Task 9: 删除旧 subkey 编码与 raw_subkey API

**Files:**
- Modify: `aikv/src/storage/subkey.rs` (删除旧函数与旧测试, 重写模块文档)
- Modify: `aikv/src/storage/types.rs` (删除 `raw_subkey_*`)
- Modify: `aikv/src/storage/adapter.rs` (删除 `raw_subkey_*` 实现)
- Modify: `aikv/src/storage/memory.rs` (如有 `raw_subkey_*` 覆盖, 删除)
- Modify: `aikv/tests/modules/storage/rename_subkey.rs` (保留仍有意义的 RENAME/COPY 用例)

- [ ] **Step 1: 删除**

- subkey.rs: 删除 `SUBKEY_SEPARATOR`, `SUBKEY_LEN_BYTES`, `encode_hash_field_key`, `encode_set_member_key`, `hash_subkey_prefix`, `set_subkey_prefix`, `reject_subkey_parent`, `reject_subkey_field`, `encode_collection_subkey`, `decode_subkey`, `extract_user_key_from_subkey`, `is_not_subkey` 及其测试. 模块文档改为:

```rust
//! 大集合 (Hash/Set) 的版本化 subkey 编码.
//!
//! ```text
//! header: {db}:{user_key}                                   值: postcard(StoredValue{CollectionHeader})
//! subkey: {db}\x01{len u32 BE}{user_key}{version u64 BE}{tag}{sub}
//!         tag = b'H' (值为 field 值) | b'S' (值为空)
//! ```
//!
//! `\x01 < b':'`, 所以 `{db}\x01` 与 `{db}:` 不共享前缀, 主键扫描看不到 subkey.
//! 只有版本与 header 相同的 subkey 可见; 其余由 compaction filter 回收.
```

- types.rs / adapter.rs / memory.rs: 删除 `raw_subkey_get/set/delete/for_each`.

- [ ] **Step 2: 编译与校验**

Run: `cargo build && cargo build --features cluster && rg -n "raw_subkey|encode_hash_field_key|encode_set_member_key|decode_subkey\b|reject_subkey" src tests`
Expected: 编译通过; `rg` 无输出.

- [ ] **Step 3: 跑测试**

Run: `cargo test --lib storage:: && cargo test --test storage -- collection_lifecycle rename_subkey --test-threads=1 && cargo test --features cluster --lib cluster_adapter`
Expected: PASS

- [ ] **Step 4: 提交 (需用户审阅)**

```bash
git add src tests
git commit -m "refactor: 删除旧 subkey 编码与 raw_subkey 接口"
```

---

### Task 10: aikv compaction filter 回收孤儿 subkey

**Files:**
- Rename: `aikv/src/storage/ttl_filter.rs` → `aikv/src/storage/compaction_filter.rs`
- Modify: `aikv/src/storage/mod.rs:33` (导出 `AikvCompactionFilter`)
- Modify: `aikv/src/main.rs:24,66-68,360`
- Modify: `aikv/tests/modules/storage/ttl_filter.rs`, `aikv/tests/proptest/ttl.rs` (类型改名)
- Create: `aikv/tests/modules/storage/subkey_gc.rs`
- Modify: `aikv/tests/modules/storage/mod.rs`

**Interfaces:**
- Consumes: aidb `CompactionPointReader`, `filter_with_reader` (Task 1); `decode_versioned_subkey`, `version_micros`, `now_micros`, `SUBKEY_GC_GRACE_US` (Task 3).
- Produces:
  - `pub struct AikvCompactionFilter` + `AikvCompactionFilter::single_node() -> Self` / `AikvCompactionFilter::cluster() -> Self`
  - `AikvCompactionFilter::with_grace_us(self, grace_us: u64) -> Self` (测试用, 缩短宽限期)

- [ ] **Step 1: 写失败的 GC 测试 (`subkey_gc.rs`)**

```rust
//! 版本化 subkey 的 compaction 回收 (真实 aidb).

use std::collections::HashMap;
use std::sync::Arc;

use aikv::storage::subkey::subkey_db_prefix;
use aikv::storage::{AiDbEngine, AikvCompactionFilter, KvStorage, KvStorageAdapter, StoredValue, ValueType};
use tempfile::TempDir;

fn open(dir: &TempDir, grace_us: u64) -> (Arc<dyn KvStorage>, Arc<aidb::DB>) {
    let engine = AiDbEngine::open_for_testing(dir.path()).expect("open aidb");
    let db = engine.db.clone();
    db.set_compaction_filter(Some(Arc::new(
        AikvCompactionFilter::single_node().with_grace_us(grace_us),
    )));
    (KvStorageAdapter::new(engine), db)
}

fn big_hash(n: usize) -> HashMap<Vec<u8>, Vec<u8>> {
    (0..n).map(|i| (format!("f{i}").into_bytes(), b"v".to_vec())).collect()
}

fn compact(db: &aidb::DB) {
    db.flush().unwrap();
    for i in 0..5u8 {
        db.put(&[b'p', i], &[i]).unwrap();
        db.flush().unwrap();
    }
    db.drain_compactions().unwrap();
}

fn subkey_count(db: &aidb::DB) -> usize {
    let prefix = subkey_db_prefix(0);
    let mut end = prefix.clone();
    *end.last_mut().unwrap() += 1;
    db.scan(Some(&prefix), Some(&end)).unwrap().count()
}

#[tokio::test]
async fn deleted_collection_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: None })
        .await
        .unwrap();
    storage.delete(0, b"h").await.unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 0);
    db.close().unwrap();
}

#[tokio::test]
async fn live_collection_subkeys_survive() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: None })
        .await
        .unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    let got = storage.get_expanded(0, b"h").await.unwrap().unwrap();
    assert_eq!(got.value, ValueType::Hash(big_hash(100)));
    db.close().unwrap();
}

#[tokio::test]
async fn replaced_version_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    for _ in 0..2 {
        storage
            .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: None })
            .await
            .unwrap();
    }
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    db.close().unwrap();
}

#[tokio::test]
async fn expired_collection_subkeys_are_collected() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 0);
    let past = aikv::storage::now_ms() - 1000;
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: Some(past) })
        .await
        .unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 0);
    db.close().unwrap();
}

#[tokio::test]
async fn orphan_subkeys_inside_grace_period_survive() {
    let dir = TempDir::new().unwrap();
    let (storage, db) = open(&dir, 3_600_000_000);
    storage
        .put_value(0, b"h", StoredValue { value: ValueType::Hash(big_hash(100)), expires_at: None })
        .await
        .unwrap();
    storage.delete(0, b"h").await.unwrap();
    compact(&db);
    assert_eq!(subkey_count(&db), 100);
    db.close().unwrap();
}
```

`DB::scan(Option<&[u8]>, Option<&[u8]>) -> Result<DbIterGuard>`, `DbIterGuard` 实现 `Iterator`, 用 `count()` 统计 `[0\x01, 0\x02)` 内可见条目.

- [ ] **Step 2: 确认失败**

Run: `cargo test --test storage subkey_gc -- --test-threads=1`
Expected: 编译失败, `AikvCompactionFilter` 未定义.

- [ ] **Step 3: 实现 `compaction_filter.rs`**

```rust
//! aikv 的 compaction filter: 删除过期主键, 回收孤儿 subkey.
//!
//! 单机模式下物理 key 就是 aikv key; 集群模式下数据面 key 为
//! `\x01sm/{gid 8B}/{aikv key}`, 其它前缀 (Raft 日志、迁移 oplog) 一律保留.
//! subkey 只在 header 存在、未过期且版本相同时保留; 版本生成后
//! `grace_us` 内的 subkey 一律保留, 给暂存写留出切换 header 的时间.

use std::cell::RefCell;

use aidb::engine::compaction::{CompactionFilter, CompactionPointReader, FilterDecision};
use aidb::engine::memtable::extract_user_key;

use crate::storage::collection::{now_micros, version_micros, SUBKEY_GC_GRACE_US};
use crate::storage::subkey::decode_versioned_subkey;
use crate::storage::types::{StoredValue, ValueType};
use crate::storage::AiDbEngine;

const SM_PREFIX: &[u8] = b"\x01sm/";
const SM_HEADER_LEN: usize = SM_PREFIX.len() + 8 + 1;

#[derive(Clone, Copy)]
enum Mode {
    SingleNode,
    Cluster,
}

pub struct AikvCompactionFilter {
    mode: Mode,
    grace_us: u64,
}

#[derive(Clone, Copy)]
enum Seen {
    Missing,
    Live(u64),
    Expired(u64),
    /// 不是集合 header (类型变了); 任何版本的 subkey 都不可见.
    Other,
}

thread_local! {
    static LAST_HEADER: RefCell<Option<(Vec<u8>, Seen)>> = const { RefCell::new(None) };
}

impl AikvCompactionFilter {
    pub fn single_node() -> Self {
        Self { mode: Mode::SingleNode, grace_us: SUBKEY_GC_GRACE_US }
    }

    pub fn cluster() -> Self {
        Self { mode: Mode::Cluster, grace_us: SUBKEY_GC_GRACE_US }
    }

    pub fn with_grace_us(mut self, grace_us: u64) -> Self {
        self.grace_us = grace_us;
        self
    }

    /// 拆成 (读 header 用的物理前缀, aikv key). 非数据面 key 返回 None.
    fn split<'a>(&self, user_key: &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
        match self.mode {
            Mode::SingleNode => Some((&[], user_key)),
            Mode::Cluster => {
                if !user_key.starts_with(SM_PREFIX) || user_key.len() < SM_HEADER_LEN {
                    return None;
                }
                Some(user_key.split_at(SM_HEADER_LEN))
            }
        }
    }

    fn expired_main_key(value: &[u8]) -> FilterDecision {
        match postcard::from_bytes::<StoredValue>(value) {
            Ok(stored) if stored.is_expired() => FilterDecision::Remove,
            _ => FilterDecision::Keep,
        }
    }

    fn observe(reader: &dyn CompactionPointReader, header_key: &[u8]) -> Option<Seen> {
        let raw = match reader.get(header_key) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Some(Seen::Missing),
            Err(_) => return None,
        };
        let stored = postcard::from_bytes::<StoredValue>(&raw).ok()?;
        Some(match stored.value {
            ValueType::CollectionHeader { version, .. } if stored.is_expired() => Seen::Expired(version),
            ValueType::CollectionHeader { version, .. } => Seen::Live(version),
            _ => Seen::Other,
        })
    }

    /// `None` 表示缓存不足以下结论, 需要重新点查.
    fn decide_cached(seen: Seen, version: u64) -> Option<FilterDecision> {
        match seen {
            Seen::Live(v) if version == v => Some(FilterDecision::Keep),
            Seen::Live(v) | Seen::Expired(v) if version < v => Some(FilterDecision::Remove),
            Seen::Expired(v) if version == v => Some(FilterDecision::Remove),
            _ => None,
        }
    }

    fn decide_fresh(seen: Seen, version: u64) -> FilterDecision {
        match seen {
            Seen::Live(v) if v == version => FilterDecision::Keep,
            _ => FilterDecision::Remove,
        }
    }
}

impl CompactionFilter for AikvCompactionFilter {
    fn filter(&self, _level: usize, key: &[u8], value: &[u8]) -> FilterDecision {
        let Some((_, aikv_key)) = self.split(extract_user_key(key)) else {
            return FilterDecision::Keep;
        };
        if decode_versioned_subkey(aikv_key).is_some() {
            return FilterDecision::Keep;
        }
        Self::expired_main_key(value)
    }

    fn filter_with_reader(
        &self,
        _level: usize,
        key: &[u8],
        value: &[u8],
        reader: &dyn CompactionPointReader,
    ) -> FilterDecision {
        let Some((space, aikv_key)) = self.split(extract_user_key(key)) else {
            return FilterDecision::Keep;
        };
        let Some(sub) = decode_versioned_subkey(aikv_key) else {
            return Self::expired_main_key(value);
        };
        if now_micros().saturating_sub(version_micros(sub.version)) < self.grace_us {
            return FilterDecision::Keep;
        }
        let mut header_key = space.to_vec();
        header_key.extend_from_slice(&AiDbEngine::encode_key(sub.db, sub.user_key));

        let cached = LAST_HEADER.with(|cell| {
            cell.borrow()
                .as_ref()
                .filter(|(k, _)| *k == header_key)
                .and_then(|(_, seen)| Self::decide_cached(*seen, sub.version))
        });
        if let Some(decision) = cached {
            return decision;
        }
        let Some(seen) = Self::observe(reader, &header_key) else {
            return FilterDecision::Keep;
        };
        LAST_HEADER.with(|cell| *cell.borrow_mut() = Some((header_key, seen)));
        Self::decide_fresh(seen, sub.version)
    }
}
```

`mod.rs`: `pub mod compaction_filter;` 替换 `pub mod ttl_filter;`, `pub use compaction_filter::AikvCompactionFilter;` 替换 `TtlExpireFilter` 导出.

`main.rs`: 单机注册处改为 `Some(std::sync::Arc::new(AikvCompactionFilter::single_node()))`, 集群 `LifecycleConfig` 处改为 `Some(std::sync::Arc::new(AikvCompactionFilter::cluster()))`, import 同步改名.

`tests/modules/storage/ttl_filter.rs` 与 `tests/proptest/ttl.rs`: `TtlExpireFilter` → `AikvCompactionFilter::single_node()` (proptest 中 `let filter = TtlExpireFilter;` 改为 `let filter = AikvCompactionFilter::single_node();`, 调用 `filter.filter(...)` 不变).

- [ ] **Step 4: 加单元测试 (compaction_filter.rs 内)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_decisions_never_drop_a_newer_version() {
        assert!(AikvCompactionFilter::decide_cached(Seen::Missing, 5).is_none());
        assert!(AikvCompactionFilter::decide_cached(Seen::Live(5), 6).is_none());
        assert!(AikvCompactionFilter::decide_cached(Seen::Expired(5), 6).is_none());
        assert_eq!(AikvCompactionFilter::decide_cached(Seen::Live(5), 5), Some(FilterDecision::Keep));
        assert_eq!(AikvCompactionFilter::decide_cached(Seen::Live(5), 4), Some(FilterDecision::Remove));
        assert_eq!(AikvCompactionFilter::decide_cached(Seen::Expired(5), 5), Some(FilterDecision::Remove));
    }

    #[test]
    fn cluster_mode_keeps_non_data_keys() {
        let filter = AikvCompactionFilter::cluster();
        assert!(filter.split(b"\x00raft/12345678/log/x").is_none());
        let mut sm = b"\x01sm/".to_vec();
        sm.extend_from_slice(&7u64.to_be_bytes());
        sm.push(b'/');
        sm.extend_from_slice(b"0:k");
        let (space, key) = filter.split(&sm).unwrap();
        assert_eq!(key, b"0:k");
        assert!(space.starts_with(b"\x01sm/"));
    }
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib storage::compaction_filter && cargo test --test storage -- subkey_gc ttl_filter --test-threads=1 && cargo test --test proptest ttl && cargo build --features cluster`
Expected: PASS

- [ ] **Step 6: 提交 (需用户审阅)**

```bash
git add src/storage src/main.rs tests
git commit -m "feat: compaction filter 回收版本不匹配或父 key 已删除的 subkey"
```

---

### Task 11: 文档、CHANGELOG 与部署验证

**Files:**
- Modify: `aikv/docs/modules/03-storage.md` (§ 文件表 `subkey.rs`/`collection.rs`/`compaction_filter.rs` 行, "Subkey 编码隔离" 要点, §3 Subkey 编码规范)
- Modify: `aikv/CONTEXT.md`
- Modify: `aikv/CHANGELOG.md`
- Modify: `aikv/docs/modules/04-commands-core.md` (OBJECT ENCODING, `*STORE` 覆盖语义, 若有相关描述)
- Modify: `/root/code/wiqun/aikv-tl-hauler-timeout-report.md` (追加测量)

- [ ] **Step 1: 文档**

- `03-storage.md` §3 重写为: 物理布局 (spec 4.1/4.2), 版本号与时钟风险 (spec 5), 暂存写与宽限期 (spec 7), 集群下 `apply_writes` 非原子的影响 (spec 7.1), GC 决策与缓存规则 (spec 10.2), SWAPDB 非原子说明. 删除 "集合删除时同时清理元数据与对应前缀的 Subkey 数据键" 这句 (已不成立).
- `CONTEXT.md` 术语: "集合版本 (collection version)", "暂存写 (staged write)", "GC 宽限期 (grace period)", 并注明 "开发阶段无存量数据, subkey 格式变更不做兼容 (2026-10-10)".
- `CHANGELOG.md` `[Unreleased]`:
  - Fixed: P1–P13 各一条 (中文, 带 Issue 号).
  - Changed: subkey 物理布局与键空间; `TtlExpireFilter` 改名 `AikvCompactionFilter`; `*STORE` 覆盖非 Set 目标; OBJECT ENCODING 对大集合返回 `hashtable`.
  - Performance: 多 field HSET/SADD 单次提交; DEL 大集合 O(1); ZSET 批量解码 (Task 0).

- [ ] **Step 2: 文档一致性检查**

Run: `rg -n "\\\\x01H|\\\\x01S|raw_subkey|TtlExpireFilter|reject_subkey" docs CONTEXT.md README.md src tests`
Expected: 只剩 CHANGELOG 中描述改名的条目.

- [ ] **Step 3: 提交 (需用户审阅)**

```bash
git add docs CONTEXT.md CHANGELOG.md
git commit -m "docs: 同步版本化 subkey 布局、GC 与集合 API"
```

- [ ] **Step 4: 部署验证 (需用户批准)**

新格式与 112/113 上已有数据不兼容, 重新部署需要清空数据目录 (破坏性操作). 执行前向用户确认:
1. 是否允许清空 112/113 的 aikv 数据目录并用本分支重新部署;
2. 部署方式沿用 `deploy/aikv-cluster.sh build --local` (确认它使用本机 `~/.cargo/config.toml` 的 aidb 本地 patch 构建).

获准后:
- 在 112 上复跑 HSET 曲线 (100/1000/5000/20000 field, 同 "追加 9" 的方法), 预期 HSET 50 field 的单命令耗时从约 5.5 ms × field 数降到一次提交级别.
- 在开发机运行 `cd /root/code/wiqun/tests/tl.hauler && dotnet test --filter "FullyQualifiedName~MsgBenchmarkTests"`, 记录耗时 (本子项目不预期过线, ZSET 优化在子项目 4).
- 结果追加到 `aikv-tl-hauler-timeout-report.md` "追加 11".

---

## Self-Review 记录

- Spec 覆盖: §4 布局 → Task 3/4; §5 版本号 → Task 3; §6 API → Task 4; §7 暂存写/宽限期 → Task 4/10; §8 键级操作 → Task 4 (FLUSHDB)、Task 5 (RENAME/COPY/MOVE/SWAPDB/DUMP/RESTORE/MIGRATE/OBJECT)、Task 6 (HSCAN)、Task 8 (KEYS/SCAN); §9 命令层 → Task 6/7; §10.1 aidb → Task 1; §10.2 filter → Task 10; §10.3 删除标记 → Task 2; §11 测试 → 各任务; §13 文档 → Task 1/2/11.
- `SSTableReader::get`, `DB::scan`, MIGRATE 取值位置已核对并写定.
- 类型一致性: `CollectionWrite`/`HeaderWrite`/`subkey_get_many(&[&[u8]])`/`subkey_scan` 返回 `(sub, value)` 在 Task 4–10 中一致; header 字段 `count: u64, version: u64` 全程一致.
