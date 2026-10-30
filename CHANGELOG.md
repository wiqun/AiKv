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

- 集群流水线内连续的纯 `SET key value` 先按序入队、批末统一等待 Raft 确认 (`storage::deferred`), 其余命令遇到时先等待前序写确认, 在保持按序的前提下恢复写吞吐 (`-P 16 -c 100` 集群 SET 约 132k QPS).
- 集群流水线内的 `EVAL` / `EVALSHA` 同样先按序执行到入队, 批末统一等待 Raft 确认 (提交后的在途清理随 `ScriptCommit` 句柄一起推迟), 后续脚本经在途覆盖层读到前序脚本未提交的写. 修复了「流水线按序」改动引入的退化: 单连接深流水线 EVAL 写吞吐在 `-P 128` 下由约 527/s 恢复并提升到约 9.4k/s (旧并发版 5.9k/s), 业务套件 `tl.hauler` 的 `TransBenchmark` 与 `SendPileUp` 恢复通过.
- 命令延迟统计 `LatencyStats`: 热路径只取读锁且不分配, 直方图每次只累加一个桶, 历史样本只记慢调用; 单机 GET 流水线由约 170k 提升到约 200k+ QPS.
- 集群 EVAL 轻脚本不再每次跨线程往返, 并发 200/500 下 S4/S5 吞吐由约 6k~9k 提升到 16k~24k.
- `groups_with_slots` 对连续相同 group 的 slot 跳过哈希插入, `derive_cluster_ok` 在 16384 slot 表上由约 80µs 降到约 14µs (本机 release 实测), 集群后台 tick 每 250ms 调用一次.
- SET/DEL 热路径: 仅当本节点有 WATCH 时才写 watch meta, 并与用户写同一次 Raft propose; meta 使用相同 hash tag 同 slot (`#83`).

### Fixed

- 集群启用时拒绝 `engine=memory`, 避免 `init_cluster` panic 与绕过 Raft 的内存写入 (`#77`).
- 修复 `MULTI/EXEC` 跨 slot 部分执行与事务期间客户端命令插队问题, 并对齐 Redis 运行时错误不回滚语义 (`#78`).
- WATCH 版本改为存储层 meta key (含 DB), 随写入 apply 递增, 支持跨连接冲突检测 (`#79`).
