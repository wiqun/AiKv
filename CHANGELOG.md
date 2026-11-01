# Changelog

本项目的所有重要变更都会记录在此文件中.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/).

## [Unreleased]

### Added

- `AIKV_SCRIPT_OFF_WORKER` 新增默认的 `auto` 取值: 按脚本 sha 记录执行耗时的 EWMA, 低于 1ms 的轻脚本就地执行, 否则派发到脚本线程池 (集群与单机一致); `1`/`true` 强制线程池, `0`/`false` 强制就地.

### Changed

- 集群模式流水线 (pipeline) 内的命令改为按到达顺序执行, 与单机一致. 之前集群为每条命令 `spawn` 并发执行, 同一连接 `SET k new` + `GET k` 会读到旧值, 连续 `SET k` 的最终值也可能不是最后一条.
- `LATENCY HISTORY` 的样本只记录耗时不低于 1ms 的调用 (与 Redis latency-monitor 只记录超阈值事件一致); 计数与直方图仍包含全部调用.

### Performance

- 大 ZSet (成员数超过 64) 的读路径不再 O(N): 直连命令 `ZSCORE` / `ZMSCORE` 改为 member 点查, `ZRANGE` / `ZREVRANGE` / `ZRANGEBYSCORE` / `ZREVRANGEBYSCORE` 只扫 score 索引中需要的条数 (支持 `LIMIT` 与反向; 按序号读从近端扫); 脚本里的 `ZRANGE*` / `ZRANGEBYSCORE*` 同样改为有界扫描并叠加在途覆盖层 (本脚本改过的 ZSet 与小集合仍走全量视图). 之前每次范围读都要加载全部成员再整体排序 (消息队列类业务脚本里的主要开销). 新增存储原语 `KvStorage::raw_range_scan` / `ScriptIo::raw_range_scan` (依赖 aidb 新增的 `scan_local_user_pairs_limited`). 家里测试机 (i5-10210U) 上 2000 成员 ZSet 的 Lua 读 (`ZRANGEBYSCORE ... LIMIT 0 32` + `ZSCORE`, 8 连接) 单机/集群由约 2.8k/2.7k 提升到约 5.7k/5.7k QPS, P50 由 2.7ms 降到 1.3ms; 剩余差距主要是同一 key 上脚本串行持有 key 锁.
- 集群流水线内连续的纯 `SET key value` 先按序入队、批末统一等待 Raft 确认 (`storage::deferred`), 其余命令遇到时先等待前序写确认, 在保持按序的前提下恢复写吞吐 (`-P 16 -c 100` 集群 SET 约 132k QPS).
- 集群流水线内的 `EVAL` / `EVALSHA` 同样先按序执行到入队, 批末统一等待 Raft 确认 (提交后的在途清理随 `ScriptCommit` 句柄一起推迟), 后续脚本经在途覆盖层读到前序脚本未提交的写. 修复了「流水线按序」改动引入的退化: 单连接深流水线 EVAL 写吞吐在 `-P 128` 下由约 527/s 恢复并提升到约 9.4k/s (旧并发版 5.9k/s), 业务套件 `tl.hauler` 的 `TransBenchmark` 与 `SendPileUp` 恢复通过.
- 命令延迟统计 `LatencyStats`: 热路径只取读锁且不分配, 直方图每次只累加一个桶, 历史样本只记慢调用; 单机 GET 流水线由约 170k 提升到约 200k+ QPS.
- 集群 EVAL 轻脚本不再每次跨线程往返, 并发 200/500 下 S4/S5 吞吐由约 6k~9k 提升到 16k~24k.
- `groups_with_slots` 对连续相同 group 的 slot 跳过哈希插入, `derive_cluster_ok` 在 16384 slot 表上由约 80µs 降到约 14µs (本机 release 实测), 集群后台 tick 每 250ms 调用一次.
- SET/DEL 热路径: 仅当本节点有 WATCH 时才写 watch meta, 并与用户写同一次 Raft propose; meta 使用相同 hash tag 同 slot (`#83`).
- 流水线延迟确认下脚本提交路径不再 `yield_now`: 之前每条脚本在忙的运行时里平均白等约 38µs, 单连接深流水线 (`-P 128`) 脚本写在集群里只有约 8k QPS; 现在约 18k (家里测试机 S6 集群 8.2k → 18.0k, 单机 16.7k 不变). 非流水线路径保留让出 (去掉会让多连接单机 S4 由 19.9k 降到 11.7k).

- 单机脚本独占执行时 (`ActiveScripts` 计数为 1 且在途覆盖层为空) 在脚本线程内同步落盘, 不再经 `ClusterDataAdapter` 的异步入队与 `spawn_blocking`; 有并发脚本时仍走异步路径 (无条件同步会让多连接 S4/S5 单机掉约 37%). 家里测试机 S6 单机 16.1k -> 约 19.7k QPS, S4/S5 单机与集群不变. 集群不受影响.

### Fixed

- 修复单机并发脚本丢更新: 未路由到 data group 的写 (单机, 带 `cluster` feature 的构建里也走 `ClusterDataAdapter`) 在 `ClusterBatchWaitHandle::wait` 时才落本地引擎, 而 `enqueue_chain` 以 `enqueue_batch` 返回为同 key 写的顺序点, 两个脚本的写按 `wait` 先后落盘时旧值会覆盖新值 (`-c 50` 同 key 计数 20000 次约 3/8 轮丢 100+ 条). 现在 `enqueue_batch` 内完成本地写.
- 修复集群并发脚本偶发丢更新: 批处理器为每个批次 `spawn` 并发 propose (最多 16 个), 同 key 先后两批可能以相反顺序进入 Raft (同 key 计数约每 16 轮丢 1 轮, 约 50 条). 现在同一 data group 同一时刻只有一个提案在途 (group commit), 提案按批次顺序进入 Raft; 代价是集群写吞吐小幅下降 (家里测试机 S1 约 -14%, S4/S5/S6 约 -6%~-7%).
- 集群启用时拒绝 `engine=memory`, 避免 `init_cluster` panic 与绕过 Raft 的内存写入 (`#77`).
- 修复 `MULTI/EXEC` 跨 slot 部分执行与事务期间客户端命令插队问题, 并对齐 Redis 运行时错误不回滚语义 (`#78`).
- WATCH 版本改为存储层 meta key (含 DB), 随写入 apply 递增, 支持跨连接冲突检测 (`#79`).
