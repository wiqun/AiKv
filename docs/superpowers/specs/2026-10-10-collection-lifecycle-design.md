# 大集合生命周期与版本化 Subkey 设计

日期: 2026-10-10
范围: aikv (主) + aidb (compaction filter 扩展)
所属: 存储优化 6 个子项目中的第 1 个

## 1. 背景

tl.hauler 的 `MsgBenchmarkTests` 在 aikv 上比 Kvrocks 慢约 5 倍. 排查结论与测量数据见
`/root/code/wiqun/aikv-tl-hauler-timeout-report.md` 的 "追加 9" 和 "追加 10".
优化分 6 个子项目, 顺序已确认:

1. 本文: 大集合生命周期修复 + 版本化 subkey (同时修掉 Hash/Set subkey 的一批 bug).
2. aidb 迭代器改造: memtable 免复制 seek, SST 范围裁剪, 高效反向迭代.
3. aikv Lua 原生读写 subkey.
4. ZSET 改为 member→score + score 索引布局, header 带 head/tail 提示.
5. aidb 墓碑密度触发 compaction (先测量再决定).
6. 三副本单写延迟排查.

子项目 1 是后续所有子项目的地基: ZSET 新布局要复用这里的版本化 subkey, 生命周期正确性也要在这里先立住.

## 2. 现状问题

Hash/Set 超过 64 个元素后转为 subkey 布局 (`CollectionHeader{kind,count}` + `{db}:{key}\x01H/S{len u16}{field}`).
当前实现有下列已确认问题:

| # | 问题 | 后果 |
|---|------|------|
| P1 | `DEL` 只删 header (`adapter.rs::delete`) | subkey 成孤儿; 同名 key 重建为大集合后旧 field 复活 |
| P2 | 惰性过期 / compaction TTL 只删 header | 同 P1 |
| P3 | `MOVE` 用 `set_typed + delete` | 目标库只有 header, 数据丢失 |
| P4 | `SWAPDB` 只搬 header | 数据丢失 + 孤儿 |
| P5 | `DUMP` 序列化 header 本身 | `RESTORE` 后数据丢失; `MIGRATE` 同样 |
| P6 | Lua 把整个集合展开后内联写回 | 旧 subkey 残留并在后续转换时复活 |
| P7 | `MemoryEngine` 不支持 raw subkey | 第 65 个 field 报错 |
| P8 | KEYS/SCAN 靠 "反序列化失败" 跳过 subkey | field 值恰好能解码成 `StoredValue` 时被当作 key 返回 |
| P9 | 多 field `HSET` 每个 field 一次读 + 一次提交 | 测得约 3 ms/field |
| P10 | `raw_subkey_get(...).unwrap_or_default()` | 读错误被吞掉, count 漂移 |
| P11 | `HSCAN` 游标越界 | `paginate_slice` 切片 panic |
| P12 | 父 key 含 `0x01` 被拒绝, field 限长 u16 | 与 Redis 不兼容 |
| P13 | `SMOVE` / `S*STORE` 不走阈值转换 | 大集合内联存储 |

### 2.1 新发现: compaction filter 删除最新 Put 会让旧版本复活 (aidb)

`aidb/src/engine/compaction/job.rs` 的 `write_sub_compaction` 中, filter 返回 `Remove` 时只是不写出该 entry.
如果同一 user key 的更老版本在本轮 compaction 之外的更深层文件里 (`surviving` 区间覆盖它),
compaction 之后读到的就是那个老版本. 删除标记已有 `drop_delete_marker` 的保护, filter 删除没有.

对 aikv 的影响: 一个 key 先 `SET` 无 TTL, 下沉到深层后再 `EXPIRE`, 过期后 TTL filter 删掉新版本, 旧的无 TTL 版本复活.
本设计把 header 的 TTL 判定合并进同一个 filter, 所以这里一并修复: filter 删除最新 Put 且存在快照或深层重叠时, 改写为同 sequence 的删除标记.
该项是新发现, 需用户确认纳入范围 (计划中为独立任务, 可单独拒绝).

## 3. 决策汇总

| 项 | 决策 |
|----|------|
| 兼容性 | 开发阶段, 无存量数据, 不做旧格式迁移或兼容读 |
| 内存引擎 | 方案 A: `MemoryEngine` 永远内联, 无阈值转换 |
| 垃圾回收 | 方案 G3: header 带版本; 版本不匹配或 header 缺失/过期的 subkey 由 compaction filter 删除 |
| 内联阈值 | 保持 64; 不回退为内联; `DBSIZE` 语义不变 (仍是引擎条目估算) |
| Lua | 本子项目只保证正确, 仍内联写回; 原生 subkey 读写留给子项目 3 |
| 超大单命令 | 撤回原 "集群下返回明确错误" 一项, 理由见 7.1; 批次字节上限移入第 12 节 |
| SWAPDB | 实现, 不拒绝 |
| OBJECT ENCODING | subkey 布局返回 `hashtable` |

## 4. Key 布局

### 4.1 Header (主键空间)

物理 key 不变: `{db}:{user_key}` (`AiDbEngine::encode_key`).

```rust
ValueType::CollectionHeader {
    kind: CollectionKind,   // Hash | Set, 子项目 4 追加 ZSet
    count: u64,
    version: u64,
}
```

`expires_at` 仍在外层 `StoredValue`.

### 4.2 Subkey (独立键空间)

```text
{db 十进制}\x01{user_key_len: u32 BE}{user_key}{version: u64 BE}{tag: u8}{sub}
```

- `tag`: `b'H'` hash field, `b'S'` set member. 子项目 4 追加 ZSET 的 tag.
- `sub`: 剩余全部字节, 不带长度; field/member 长度不再受 u16 限制.
- Hash 的 value 是 field 值; Set 的 value 为空.
- `0x01 < b':'`, 所以 `{db}\x01` 与 `{db}:` 不共享前缀: KEYS/SCAN/INFO/RANDOMKEY 只扫 `{db}:` 就天然看不到 subkey (修 P8).
- `user_key` 带长度前缀, 父 key 可以含任意字节 (修 P12).
- 同一集合同一版本的 subkey 在物理上连续, `subkey_scan` 是一次前缀扫描.
- `FLUSHDB` 删两个区间: `[{db}:, {db};)` 与 `[{db}\x01, {db}\x02)`.

### 4.3 集群路由

`ClusterDataAdapter` 路由需要 user key. 新增 `subkey::routing_user_key(physical) -> Option<Vec<u8>>`:
subkey 解出 `user_key`, 主键走 `AiDbEngine::decode_key`. `route_read` / `route_write` / `key_slot_status` 三处统一改用它,
删除硬编码 H/S 的 `strip_subkey_suffix`.

注: aidb slot 迁移按物理 key 算 slot, 不带 hash tag 时与 aikv 路由不一致. 这是既有问题, 不在本子项目范围 (见第 12 节).

## 5. 版本号

```text
version = max(last + 1, now_us << 11)
```

- 进程内全局 `AtomicU64`, 由执行写命令的节点 (集群中即 slot 所在 group 的 leader) 生成, 随 Raft entry 复制; 副本不自行生成.
- 低 11 位是同一微秒内的序号, 每微秒可生成 2048 个版本.
- 仅在 "新建 subkey 集合" 时分配新版本: 内联转 subkey, RENAME/COPY/MOVE/RESTORE/`*STORE`/SWAPDB 写入目标.
  对已存在 header 的增删改沿用原版本.
- `version >> 11` 是生成时刻 (µs), 供 GC 宽限期使用.

时钟风险: 进程重启或 leader 切换后若时钟回拨, 新版本可能小于旧版本. 只有新旧版本恰好相等时旧垃圾才会重新可见,
需要回拨量与 2048 序号同时撞上, 概率极低. 运维要求开启 NTP; 此风险写入 `docs/modules/03-storage.md`.

## 6. 存储层 API

替换 `KvStorage::raw_subkey_*` 四个方法. 新代码放在 `src/storage/collection.rs`, `adapter.rs` 只做委托 (该文件已超过 800 行, 不再增长).

```rust
pub const HASH_MAX_INLINE_FIELDS: usize = 64;
pub const SET_MAX_INLINE_MEMBERS: usize = 64;

pub enum HeaderWrite {
    /// 改写已有 header (TTL 计数净变化为 0, 与 `rewrite_stored` 一致).
    Put(StoredValue),
    Delete { had_ttl: bool },
}

pub struct CollectionWrite {
    pub header: HeaderWrite,
    pub version: u64,
    pub kind: CollectionKind,
    pub puts: Vec<(Vec<u8>, Vec<u8>)>,
    pub deletes: Vec<Vec<u8>>,
}

// KvStorage 新增方法 (默认实现见下)
fn supports_subkeys(&self) -> bool;                       // 默认 false
async fn subkey_get_many(&self, db, key, version, kind, subs: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>>;
async fn subkey_scan(&self, db, key, version, kind) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
async fn commit_collection(&self, db, key, write: CollectionWrite) -> Result<()>;
async fn put_value(&self, db, key, value: StoredValue) -> Result<()>;      // 默认 set_typed
async fn get_expanded(&self, db, key) -> Result<Option<StoredValue>>;      // 默认 get_typed
async fn move_key(&self, src_db, dst_db, key) -> Result<bool>;
```

- `subkey_get_many`: 逐个点查 (仓库未引入 `futures`, 本地点查为进程内读, 顺序执行足够); 读错误立即返回.
- `commit_collection`: subkey puts/deletes 在前, header 与 watch meta 在后, 一次 `apply_writes` (修 P9, P10).
- `put_value`: 写整个值. Hash/Set 超过阈值且 `supports_subkeys()` 时分配新版本, 走暂存写 (第 7 节); 否则 `set_typed`.
  RESTORE、`S*STORE`、SMOVE 目标、HSET/SADD 内联增长越过阈值, 全部走它 (修 P13).
- `get_expanded`: header 展开成完整内联 `Hash`/`Set`. DUMP/MIGRATE/Lua 读路径使用 (修 P5, P6 的读侧).
- `subkey_*` 和 `commit_collection` 的默认实现返回 `Err`; `MemoryEngine` 从不产生 header, 也就不会调用它们 (修 P7).

## 7. 暂存写与宽限期

产生新版本的写入分两步:

1. 新版本 subkey 按每批 256 条分批 `apply_writes`, 每批等待完成. 此时 header 仍指向旧版本或不存在, 新 subkey 不可见.
2. 再一次 `apply_writes` 写入新 header (以及删除源 header、watch meta).

正确性只依赖顺序: 步骤 2 开始前步骤 1 的所有写已确认. 中途失败不会出现半个集合; 不需要事务.
副作用: 步骤 1 写入的 subkey 在步骤 2 之前看起来就是 "header 版本不匹配" 的垃圾.

### 7.1 集群下 `apply_writes` 不是原子的

`ClusterDataAdapter::apply_writes` 把每个 op 单独送入 group 的 `GroupSetBatcher`, 批处理器按最多 512 条
(并与其它连接的写混合) 打包成 Raft entry. 因此一次 `apply_writes` 可能落在多个 entry 里.
本设计的影响与处理:

- 暂存写: 只依赖顺序, 不受影响.
- `commit_collection` (改已有集合): subkey 在前, header 在后. 若在两个 entry 之间崩溃或换主, 客户端收到错误, 已写的 subkey 可见而 `count` 未更新. 与现有 `apply_stored_batch` 同类, 不在本次修复.
- RENAME: 目标 header 写入与源 header 删除可能分属两个 entry; 中途失败时两个 key 同时存在, 各自指向自己的版本, 不丢数据.
- 原计划的 "单命令超 8 MiB 报错" 前提不成立 (一次命令不等于一个 entry), 撤回; 批处理器是否按字节限批移入第 12 节核实.

宽限期: GC filter 对 `now_us - (version >> 11) < SUBKEY_GC_GRACE_US` 的 subkey 一律保留,
`SUBKEY_GC_GRACE_US = 3_600_000_000` (1 小时). 1 小时同时覆盖副本时钟偏差与长时间暂存.
失败残留的暂存 subkey 在宽限期后被回收.

## 8. 键级操作语义

| 操作 | subkey 布局下的做法 |
|------|---------------------|
| DEL / UNLINK | 只删 header, O(1). 旧版本 subkey 变垃圾 (修 P1) |
| 惰性过期 / compaction TTL | 只删 header (修 P2) |
| EXPIRE / PERSIST / TTL | 改写 header, 版本不变 |
| RENAME / RENAMENX / COPY | 读源 subkey → 新版本暂存到目标 → 一次写目标 header (+ 删源 header). 目标原有 subkey 变垃圾 |
| MOVE | 新增 `move_key`, 走与 RENAME 相同的跨库迁移 (修 P3) |
| SWAPDB | 两边的 subkey 集合先各自以新版本暂存到对侧, 再删两个主键区间, 最后写交换后的值 (修 P4) |
| FLUSHDB / FLUSHALL | 删主键区间 + subkey 区间 |
| DUMP / MIGRATE | `get_expanded` 后编码, payload 格式不变 (修 P5) |
| RESTORE | `put_value` |
| TYPE | 不变 |
| OBJECT ENCODING | header → `hashtable`; 内联 → `listpack` |
| KEYS / SCAN / RANDOMKEY / INFO keyspace | 只扫主键空间 (修 P8) |
| HSCAN / SSCAN | 游标越界返回空页 (修 P11). 分页性能不在本次范围 |

SWAPDB 中 "删区间 → 写回" 两步之间不原子, 与现状一致, 文档注明.

## 9. 命令层

- Hash/Set 命令只通过第 6 节 API 访问存储. 删除 `raw_subkey_*`、`migrate_*_to_subkey`、`delete_all_hash_subkeys`、`reject_subkey_parent`、`reject_subkey_field`.
- 典型路径 (header 已存在的 HSET):
  `get_typed` → `subkey_get_many` (本地点查) → 计算新增数 → `commit_collection` (一次写入调用, 原来是每个 field 一次).
- 读错误一律向上传播, 不再 `unwrap_or_default` (修 P10).
- `count` 变成 0 时 `HeaderWrite::Delete`.
- 内联集合增长越过阈值时调用 `put_value` 完成转换.

## 10. 垃圾回收 (G3)

### 10.1 aidb 扩展

```rust
pub trait CompactionPointReader: Send + Sync {
    /// 点查 user key 的最新可见值.
    fn get(&self, user_key: &[u8]) -> Result<Option<Vec<u8>>>;
}

pub trait CompactionFilter: Send + Sync {
    fn filter(&self, level: usize, key: &[u8], value: &[u8]) -> FilterDecision;
    fn filter_with_reader(
        &self, level: usize, key: &[u8], value: &[u8],
        reader: &dyn CompactionPointReader,
    ) -> FilterDecision {
        let _ = reader;
        self.filter(level, key, value)
    }
}
```

- 契约从 "纯决策, 不发起 I/O" 放宽为 "可以通过 reader 点查, 不得写入".
- `CompactionJob::run_with_reader(file_numbers, Option<&dyn CompactionPointReader>)`; 原 `run` 等价于传 `None`.
  子任务运行在 `std::thread::scope` 中, 可直接借用 reader.
- `run_compaction_once` 构造借用 `&DB` 的 `DbPointReader`, 内部调用 `get_at_sequence(key, 当前 sequence)`, 不计入 Get 操作统计.
  调用 `job.run_with_reader` 期间不持有 `sstables`/`version_set`/`write_lock`/`memtable` 的任何锁 (已核对 `run_compaction_once`), 不会死锁.
- 估算路径 (`count_dedup_*`) 仍调用无 reader 的 `filter`.

### 10.2 aikv filter

`TtlExpireFilter` 改名为 `AikvCompactionFilter`, 有两种 key 形态:

- 单机: 物理 key 即 aikv key.
- 集群: 每个 group 独立 DB, 数据面 key 为 `\x01sm/{gid 8B}/{aikv key}`; 其它前缀 (`\x00raft/`, `\x02mig/` 等) 一律 `Keep`.
  这也顺带消除了现状中 TTL filter 试图把 Raft 日志 value 当 `StoredValue` 解码的隐患.

决策:

1. 从 InternalKey 取 user key (`aidb::engine::memtable::extract_user_key`), 按模式剥出 aikv key 和读 header 所需的前缀.
2. aikv key 是主键: 能解码为 `StoredValue` 且已过期 → `Remove`, 否则 `Keep`.
3. aikv key 是 subkey:
   - 处于宽限期 → `Keep`.
   - 查 header (`{前缀}{db}:{user_key}`):
     - 读错误或解码失败 → `Keep`;
     - 不存在 → `Remove`;
     - 已过期 → `Remove`;
     - 是 `CollectionHeader` 且版本相等 → `Keep`;
     - 其它 (版本不同或类型不同) → `Remove`.
4. `filter()` (无 reader) 只做第 2 步, 对 subkey 一律 `Keep`.

线程局部缓存 (同一集合的 subkey 连续出现, 避免每条都点查). 缓存一条 `(header_key, 观测结果)`, 观测结果为 `Missing` / `Live(v)` / `Expired(v)`:

| 缓存 | subkey 版本 s | 决策 |
|------|---------------|------|
| `Live(v)` | `s == v` | `Keep` |
| `Live(v)` 或 `Expired(v)` | `s < v` | `Remove` |
| `Expired(v)` | `s == v` | `Remove` |
| `Missing`, 或 `s > v` | — | 重新点查并更新缓存 |

正确性依据: header 版本只增不减; 过期 header 的版本永久失效 (过期后再写必然分配新版本). 因此缓存过时只会多保留, 不会误删.

### 10.3 Removal → 删除标记 (第 2.1 节问题的修复)

`write_sub_compaction` 中, 最新版本为 `TypePut` 且 filter 返回 `Remove` 时:
若 `min_snap_seq != u64::MAX` 或 `surviving` 区间覆盖该 user key, 写出 `(user_key, seq, TypeDelete)` 空值删除标记; 否则照旧丢弃.
`filter_removed_user_keys` 记录不变.

## 11. 测试

- 每个 P1–P13 一个回归测试, 函数上方 `///` 中文注释写明现象、期望、Issue 编号 (编号由用户提供).
- 命令测试在两种引擎上各跑一遍: `MemoryEngine` 与 `KvStorageAdapter` + 内存 `MapAdapter` (测试辅助, 从 `rename_subkey.rs` 抽出共享).
- GC 测试用真实 `AiDbEngine` (临时目录): 删除/过期/转换后 flush + `drain_compactions`, 断言 subkey 区间为空; 宽限期内与版本匹配的 subkey 保留.
- aidb: reader 传递、删除标记改写的单元测试; 带 reader 的 filter 在 compaction 期间与并发读写不死锁.
- 集群: `routing_user_key` 单元测试; `--features cluster` 编译通过与 `cluster_adapter` 相关单测.
- 最小范围原则: 只跑受影响的测试目标, 每次汇报实际命令.

## 12. 范围外 (单独记录 Issue)

- 集群 slot 迁移: aidb 按物理 key 计算 slot, 不带 hash tag 时与 aikv 路由不一致.
- ZSET 语义缺口 (ZADD 选项、NaN 等), 子项目 4 处理布局, 语义另立.
- HSCAN/SSCAN 分页性能 (当前全量加载后切片).
- List 扁平化.
- UNLINK 异步释放 (当前与 DEL 同义, 新设计下 DEL 已是 O(1)).
- `GroupSetBatcher` 只按条数 (512) 限批, 需核实是否会拼出超过 aidb `max_entry_size` (8 MiB) 的 entry.
- 集群 `apply_writes` 非原子导致的 `count` 漂移 (见 7.1).

## 13. 文档同步

- aikv `docs/modules/03-storage.md`: subkey 编码章节重写, GC 与宽限期, 版本时钟风险, SWAPDB 非原子说明.
- aikv `CONTEXT.md`: 术语 "集合版本", "暂存写", "宽限期".
- aikv `CHANGELOG.md` `[Unreleased]`: Fixed / Changed / Performance.
- aidb `docs/modules/02-engine-storage.md`: filter 契约放宽, `CompactionPointReader`, 删除标记改写.
- aidb `CHANGELOG.md`.
- 本文记录 "无存量数据, 不做兼容" 的决策.
