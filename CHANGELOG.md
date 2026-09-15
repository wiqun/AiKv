# Changelog

本项目的所有重要变更都会记录在此文件中.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/).

## [Unreleased]

### Added

- `deploy/loadgen/`: 交互式加压工具 (Rust 独立 crate), 单页控制台设参后点启动, 支持单机/集群, 不含结果统计.

### Changed

- deploy: 一级入口改为 `aikv-single.sh` / `aikv-cluster.sh` / `observability.sh` / `loadgen.sh` (`build|up|down`, 容器栈无 `status`); 删除 `build-image.sh` / `up-*.sh` / `status.sh` / `down.sh`.
- loadgen: 控制台 html/css/js 从 `web/` 磁盘读取; 默认值与预设来自 `loadgen.example.toml`, 本机 `loadgen.toml` 不进仓库.
- loadgen: 探活改为 30s; 启动前拒绝不健康目标; 全节点不可达或 CLUSTERDOWN 时暂停派发、保留 worker.
- loadgen: 改表单不影响正在跑的任务; 只有点启动才按当前表单开新任务 (已在跑则先停再起), 不做差值缩放.
- loadgen: 默认限速 3000 ops/s、连接数 6.
- loadgen: 控制台目标改为 IP/端口拆分, cluster 只需一个 seed; 失败改为底部提示.

### Performance

- SET/DEL 热路径: 仅当本节点有 WATCH 时才写 watch meta, 并与用户写同一次 Raft propose; meta 使用相同 hash tag 同 slot (`#83`).

### Fixed

- 集群启用时拒绝 `engine=memory`, 避免 `init_cluster` panic 与绕过 Raft 的内存写入 (`#77`).
- 修复 `MULTI/EXEC` 跨 slot 部分执行与事务期间客户端命令插队问题, 并对齐 Redis 运行时错误不回滚语义 (`#78`).
- WATCH 版本改为存储层 meta key (含 DB), 随写入 apply 递增, 支持跨连接冲突检测 (`#79`).
