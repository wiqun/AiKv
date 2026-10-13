# ZSET Subkey 扁平化设计 (草案, 未实施)

> 状态: 草案. 用户已批准 "ZSET Subkey 扁平化作为主攻方向, 先出设计与兼容性评估, 暂不改代码" (2026-10-12).
> 本文档不包含任何已实施的代码改动. 所有性能数字以实测为准, 估算会明确标注.

## 1. 背景与目标

### 1.1 为什么要做

tl.hauler 的消息队列 (MsgQ) 通过 Lua 脚本完成收发, 脚本内的热路径是 ZSET 命令 (按时间分段的 key2, 上限 1000 成员) 与少量 Hash 写.
本地单机 aikv 跑 `MsgBenchmarkTests.SendPileUp` (10000 条, `high-write` 预设, WSL), 脚本内分段计时 (2026-10-12, 临时插桩, 已还原) 结果如下:

| 环节 | 每脚本次数 | 每次耗时 | 占脚本耗时 |
|---|---|---|---|
| ZSET 读 (ZREVRANGEBYSCORE / ZCARD / ZREVRANGE 等) | 1.38 | 63.6 µs | 约 30% |
| ZSET 写 (ZADD / ZREM) | 1.00 | 57.1 µs | 约 20% |
| Hash 写 (HSET / HDEL) | 3.00 | 23.6 µs | 约 24% |
| 提交 (commit) | - | 约 5 µs | 约 2% |
| Lua 虚拟机及其他 | - | - | 约 15% |

脚本平均耗时 290 µs, 客户端观察到约 489 µs/脚本 (4.888 秒 / 10000 次).
同一测试在 112 集群上为 6.7–7.2 秒, Kvrocks 基线为 1.2 秒 (`CloudConfigsForTest.cs` 注释与追加 10).

ZSET 整值存储是当前最大的单点成本: 每次 ZSET 命令都要整体读取、反序列化, 写命令还要整体重新编码.

### 1.2 目标

- ZSET 单命令的存储访问从 O(成员数) 降为 O(log N) (读 `ZSCORE`/`ZCARD`/范围首尾, 写 `ZADD`/`ZREM`/`ZINCRBY`/`ZPOPMIN`/`ZPOPMAX`).
- 脚本内的 ZSET 访问不再整体展开集合 (当前 `fetch_stored` 走 `get_expanded`), 也不再为每个脚本重建 `ScoreIndex`.
- 不退化当前已经 O(N) 的命令 (`ZRANK`, `ZCOUNT`, `ZRANGEBYLEX`, 集合运算).
- 收益必须体现在通用服务上, 不做针对 tl.hauler 的特调.

### 1.3 非目标 (本轮不做)

- 不改造 Hash/Set 的内联与子键规则.
- 不引入 order-statistic 结构 (`ZRANK` 的 O(log N) 实现), 首期保持 O(rank) 扫描.
- 不处理 `BZPOPMIN`/`BZPOPMAX` 的阻塞唤醒语义变更, 只保证其读写路径改用新布局后语义不变.
- 不解决集群 `apply_writes` 非原子问题 (已有记录, 见 `docs/modules/03-storage.md` 第 7.1 节).

## 2. 现状 (代码事实)

| 项 | 现状 | 位置 |
|---|---|---|
| 存储形态 | `StoredValue::ZSet(BTreeMap<Vec<u8>, f64>)`, 整值 | `src/storage/types.rs` |
| 反序列化 | 已优化为批量建树 (追加 7), 仍是 O(N) | `deserialize_zset_members` |
| 子键化 | `CollectionKind` 只有 `Hash`/`Set`, ZSET 不走子键 | `src/storage/types.rs:88`, `src/storage/subkey.rs` |
| 子键 tag | `HASH_TAG = b'H'`, `SET_TAG = b'S'` | `src/storage/subkey.rs:17-19` |
| 脚本读取 | `fetch_stored` 调用 `storage.get_expanded`, 对子键集合整体展开 | `src/command/script/transaction.rs` |
| 脚本索引 | 每个被触碰的 ZSET 构建一次 `ScoreIndex` (`zset_index`), O(N) | `src/command/script/transaction.rs` |
| 反向扫描 | aikv 存储层只有前向 `scan_prefix` / `scan`; aidb 迭代器有 `prev()` | `src/storage/adapter.rs`, `aidb/src/engine/db/iterator.rs` |

## 3. 布局设计

### 3.1 Header (主键空间)

- 沿用 `{db}:{user_key}` → `CollectionHeader { kind, count, version }`.
- `CollectionKind` 新增 `ZSet` 变体 (postcard 变体序号追加在末尾, 不改变已有变体的序号).
- `count` 为成员数, 使 `ZCARD` 为 O(1).

### 3.2 子键 (独立键空间)

沿用现有子键前缀 `{db}\x01{user_key_len u32 BE}{user_key}{version u64 BE}{tag}{sub}`, 新增两个 tag:

| tag | 用途 | sub | value |
|---|---|---|---|
| `Z` | member → score 映射 (点查, `ZSCORE`, `ZADD` 判断旧分数) | `member` 原始字节 | `score` 的 8 字节 f64 大端 |
| `R` | 按分数排序的索引 (范围查询, 首尾取值) | `sortable(score)` 8 字节大端 + `member` | 空 |

分数的可排序编码 `sortable(score)`:

- 先把 `-0.0` 规范化为 `+0.0`.
- `bits = f64::to_bits(score)`; 若符号位为 1 则 `!bits`, 否则 `bits ^ (1 << 63)`, 结果按大端写入.
- `±inf` 直接参与编码, 保证 `-inf < 有限值 < +inf`.
- `NaN` 需要在命令层拒绝 (需要确认现有 `ZADD` 是否已经拒绝; 若未拒绝, 属于既有问题, 需单独记录).
- 同分数的成员按 `member` 字节序排序, 与 Redis 语义一致.

### 3.3 内联阈值

- 成员数 ≤ `ZSET_MAX_INLINE_MEMBERS` (建议 64, 与 `HASH_MAX_INLINE_FIELDS`/`SET_MAX_INLINE_MEMBERS` 一致) 时保持 `StoredValue::ZSet` 内联整值, 不做子键化, 避免小集合的写放大.
- 超过阈值后迁移到子键布局, 参照现有 `migrate_hash_to_subkey` 的做法.
- 内联编码 (`StoredValue::ZSet` 的 postcard 字节) 保持不变, 已有字节兼容测试 `zset_bulk_decode_keeps_wire_format` 继续有效.

### 3.4 集群路由

子键的路由依旧取 `routing_user_key`, 与同一 user key 的 header 同 slot. 不影响 hash tag 与 MOVED/ASK 规则.

## 4. 命令到存储访问的映射

| 命令 | 存储访问 | 复杂度 | 备注 |
|---|---|---|---|
| `ZADD` (单成员) | `get Z(m)`; 若分数变化则 `del R(old)`; `put Z(m)`, `put R(new)`; 更新 header | O(log N) | 批量 ZADD 合并为一次 `CollectionWrite` |
| `ZREM` | `get Z(m)`; `del Z(m)`, `del R(score,m)`; 更新 header | O(log N) | |
| `ZINCRBY` | 同 `ZADD`, 分数由旧值计算 | O(log N) | |
| `ZSCORE` | `get Z(m)` | O(log N) | |
| `ZCARD` | 读 header `count` | O(1) | |
| `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` (含 `LIMIT`) | `R` 的范围扫描, 正向或反向 | O(log N + 返回条数) | 需要新增反向范围扫描接口 |
| `ZRANGE` / `ZREVRANGE` (按下标) | `R` 扫描, 跳过 `start` 条 | O(start + 返回条数) | 大偏移量退化, 需在文档中注明 |
| `ZPOPMIN` / `ZPOPMAX` | `R` 的首/尾一条 | O(log N) | |
| `ZRANK` / `ZREVRANK` | `R` 扫描计数 | O(rank) | 与现状持平 (现状也是 O(N)) |
| `ZCOUNT` | `R` 区间扫描计数 | O(区间成员数) | 与现状持平 |
| `ZRANGEBYLEX` / `ZLEXCOUNT` | 全量扫描 `Z`/`R` | O(N) | 与现状持平 |
| `ZSCAN` | `Z` 的子键扫描 + 游标 | 与 Hash 的 `HSCAN` 相同 | 需确认 `scan_cursor.rs` 的复用方式 |
| `ZUNION`/`ZINTER`/`ZDIFF` (含 `*STORE`) | 全量扫描输入 | O(N) | 与现状持平 |
| `BZPOPMIN`/`BZPOPMAX` | 唤醒后走 `ZPOPMIN`/`ZPOPMAX` 路径 | 同上 | 阻塞注册表不变 |

## 5. 脚本事务 (关键风险点)

这是本设计最需要谨慎的部分. 如果只改普通命令路径, 脚本内的访问会反而变慢.

### 5.1 现状问题

- `fetch_stored` 对任何集合都调用 `get_expanded`, 子键化后会把 1000 个成员整体展开到内存, 比现在更慢.
- `zset_index` 每个脚本重建一次 `ScoreIndex`, O(N).

### 5.2 方案

- 为子键化的 ZSET 引入 `ZSetView`: 对 `Z`/`R` 的点查与范围扫描直接访问存储, 并叠加脚本缓冲 (`write_buffer`/`overlay`) 中尚未提交的 put/delete.
- 范围扫描结果与缓冲中的增删需要做**有序合并**: 缓冲中的删除要屏蔽存储中的同名条目, 缓冲中的新增要按 `R` 的排序键插入正确位置.
- 内联小 ZSET 继续走现有路径, `zset_index` 只对内联 ZSET 保留.
- 脚本中的 `ZADD`/`ZREM` 只更新缓冲, 不做整体展开; 提交阶段仍然一次 `CollectionWrite`.

### 5.3 验证要求

- 属性测试: 随机生成 `ZADD`/`ZREM`/`ZINCRBY`/范围查询/`ZPOPMIN` 序列, 与 `BTreeMap` 模型逐步比对, 同时覆盖脚本路径与普通命令路径.
- 边界测试: `(` 开区间, `-inf`/`+inf`, `LIMIT offset count` 的负数与越界, 同分数成员的顺序, 缓冲中删除后重新添加.

## 6. 写入与原子性

- 每条命令产生一个 `CollectionWrite` (puts + deletes + header), 复用现有暂存写与宽限期机制 (`commit_collection_impl`).
- 单个 ZADD 会写 `Z` + `R` + header, 至少 2-3 个 KV 写. 小 ZSET 因为内联而避免了这部分放大.
- 集群下 `apply_writes` 非原子: 与 Hash 子键相同的已知限制, 需要显式接受或另行修复 (见 `docs/modules/03-storage.md` 7.1).

## 7. 兼容性评估

| 项 | 影响 | 处理方式 |
|---|---|---|
| 持久化编码 (`CollectionKind`) | 新增 `ZSet` 变体, 子键 tag `Z`/`R` 新增 | 旧数据无兼容 (CONTEXT.md 决定: 开发阶段无存量数据). 不写迁移层 |
| 内联 `StoredValue::ZSet` 字节 | 不变 | 现有字节兼容测试继续生效 |
| 112/113 上已有 ZSET 数据 | 旧布局在新版本下不可读 | 部署前需清空 (**破坏性操作, 需你批准**), 或实现一次性迁移 (不推荐) |
| DUMP/RESTORE | 内部 postcard 格式需同步子键展开/折叠 | 参照 `src/storage/dump.rs` 对 Hash 的处理, 补回归测试 |
| RENAME / MOVE / SWAPDB | 需要纳入 ZSET 的集合迁移 | 参照 `move_key_impl`, `swap_db_impl` 对 Hash 的做法 |
| TTL / EXPIRE | 由 header 承载, 与 Hash 相同 | 无额外改动 |
| WATCH | 依赖键版本机制 | 需确认子键写入是否会推进 header 对应的键版本 |
| KEYS / SCAN / DBSIZE | 必须跳过子键 | 已有 `decode_versioned_subkey`, 需补测试 |
| 集群路由 / slot | 子键与 header 同路由键 | 无变化, 需补测试 |
| compaction GC | `AikvCompactionFilter` 需识别 `Z`/`R` 子键 | 复用现有宽限期与删除标记逻辑, 增加 tag 覆盖测试 |
| 文档 | `03-storage.md` 第 115 行关于 ZSet 的描述与代码不符 (追加 6), 本次需一并修正 | 实施时同步更新 `03-storage.md`, `04-commands-core.md`, `CONTEXT.md`, `CHANGELOG.md`, `ARCHITECTURE.md` |
| aidb 依赖 | 反向范围扫描需要 aikv 存储层新增接口, 底层 `prev()` 已存在 | 确认 aidb 对外是否已暴露反向迭代, 若没有需要 aidb 侧配套改动 |

## 8. 风险

1. **脚本内 `ZSetView` 的合并正确性** (高): 需要属性测试兜底, 作为合并前的门禁.
2. **写放大**: 大 ZSET 的每次 ZADD 从一次整值写变为 2-3 次 KV 写. 通过内联阈值控制小集合.
3. **O(rank) 类命令**: `ZRANK`/`ZREVRANK`/`ZRANGE` 大偏移量仍然线性. 与现状持平, 但需在文档中说明.
4. **集群非原子 apply**: 同 Hash 的已知限制, 需要明确接受或修复.
5. **固定开销**: Hash 写每次 23.6 µs 的固定开销说明存储读改写路径本身有成本, ZSET 子键化后单次成本会更接近这个下限, 不一定能降到 10 µs 以内.

## 9. 预期收益与验证门槛

### 9.1 估算 (未实测)

- 当前 ZSET 每次调用约 57–64 µs. 子键化后, 单次 ZSET 调用估计在 20–30 µs (主要是固定开销与 2-3 次点查/写入).
- 每个脚本约 2.4 次 ZSET 调用, 估计每脚本节省约 80–100 µs, 本地 `SendPileUp` 估计从 4.9 秒降到约 3.5–4.0 秒.
- 仅靠 ZSET 子键化 **达不到 Kvrocks 的 1.2 秒**. 还需要处理 Hash 写的固定开销 (约 24%) 与客户端侧约 200 µs/脚本的开销.

### 9.2 Spike 门槛 (实施前必须先做)

先做一个最小 spike (只覆盖 `ZADD`/`ZREM`/`ZCARD`/`ZREVRANGEBYSCORE LIMIT` 与脚本内的 `ZSetView`), 在 1000 成员的热点 ZSET 上测量:

- 单次 ZSET 调用 ≤ 25 µs (测量口径与分段插桩一致).
- 本地 `SendPileUp` 的脚本总耗时较基线下降 ≥ 25%.

未达到门槛则停止, 回到设计阶段重新评估, 不进入全量实施.

## 10. 分阶段计划 (草案)

1. **Spike**: 最小子集 + 脚本 `ZSetView`, 测量门槛 (见 9.2). 不提交.
2. **存储层**: `CollectionKind::ZSet`, 子键编码/解码, 分数排序编码, 迁移与 GC 覆盖, 反向范围扫描接口.
3. **命令层**: 全部 ZSET 命令切换到新访问路径, 脚本路径切换到 `ZSetView`.
4. **兼容与文档**: DUMP/RENAME/MOVE/SWAPDB/WATCH/KEYS/SCAN 回归测试, 文档同步.
5. **验证**: 单元/属性/集成测试 (按仓库的 `--test-threads=1` 约定), 本地与 112 的 `SendPileUp` 对比, 报告与 Kvrocks 基线对照.

## 11. 已确认的决策 (2026-10-12)

1. 小 ZSET 内联阈值: **64** (与 Hash/Set 一致).
2. `ZRANK`/`ZREVRANK`/`ZCOUNT`/`ZRANGEBYLEX`: **首期接受保持 O(N)**, 不做 order-statistic 结构.
3. 下一步: **先做 spike** (见第 9.2 节), 达到门槛再全量实施; spike 不提交.
4. 112/113 现有 ZSET 数据: **用户已批准在部署新布局前清空** (实际执行时仍需按破坏性操作流程在当时再次确认).

## 12. 仍待确认的事项

1. 集群 `apply_writes` 非原子问题是否在本轮接受为已知限制?
2. Hash 写固定开销优化 (脚本事务内复用展开结果, 不改编码) 是否作为第二阶段紧随其后?

## 13. Spike 结果 (2026-10-12, in-process 真实 aidb, release, 1000 成员)

测量代码: aikv worktree `/tmp/aikv-spike` 的 `tests/modules/storage/zset_spike.rs` (未提交). 子键用 Hash tag 作代理.

| 原语 | 当前实现 (spawn_blocking) | 诊断: 去掉 spawn_blocking |
|---|---|---|
| 整值 ZADD 改分 (get 整表 + set 整表) | 388 µs | 408 µs |
| 整值 ZCARD (get 整表) | 103 µs | 76 µs |
| 子键 ZADD 改分 (header读 + Z读 + commit) | 133 µs | **20 µs** |
| 子键 ZCARD (读 header) | 34 µs | 0.6 µs |
| 子键 ZREVRANGEBYSCORE LIMIT 0 1 (反向 seek) | 47 ms | 47 ms |
| 小 Hash get_typed (3 field) | 61 µs | 0.7 µs |
| 对照: raw aidb db.get | 0.5 µs | 0.5 µs |

结论:

1. **固定开销来自 aikv 适配层**: `AiDbEngine::blocking` 对每次访问都做 `spawn_blocking` (`src/storage/aidb.rs`), 单次约 30 µs. 这项开销影响**所有命令** (包括现有 Hash 的 HGET/HSET), 是 Hash 写 "23.6 µs 固定开销" 的来源. 去掉后子键 ZADD 降到 20 µs, 满足门槛 (≤25 µs).
2. **反向范围扫描不可用**: `db.scan` 每次约 47 ms, 与 memtable 是否为空无关. 原因在 aidb `DBIterator::new`: 对每一层 (memtable / immutable / 每个 SST) 建立 iterator, 并收集全部 range tombstone, 然后才 seek; 成本与层数和数据量相关, 不随查询区间缩小. 按分数索引 (`R`) 的设计在当前 aidb 上不成立. 需要 aidb 侧改为惰性、按区间构造的 iterator, 或改用其它结构.
3. 门槛判定: 单次 ZSET 写原语 (ZADD) 在去掉 `spawn_blocking` 后达标; 反向范围查询 (ZREVRANGEBYSCORE) 未达标, 阻塞设计. 两项都需要决策后才能继续 (见下).

需要决策:

- A. 是否接受去掉/收窄 `spawn_blocking` (例如只对 scan 与写入保留, 点查直接在调用线程执行). 这是全局行为变更, 影响并发与 tokio 工作线程阻塞, 需要单独压测.
- B. aidb 的 range iterator 惰性化改造 (跨仓库, 属于 aidb 的存储内核改动), 或暂缓 ZSET 子键化, 先做 A.
