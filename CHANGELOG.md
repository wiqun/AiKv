# Changelog

本项目的所有重要变更都会记录在此文件中.

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/),
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/).

## [Unreleased]

### Added

- `deploy/README.md`: 增加部署模块总览与架构说明, 涵盖单机, 集群, 可观测性与压测工具的拓扑关系及快速上手链路.
- `deploy/loadgen/`: 交互式加压工具 (Rust 独立 crate), 单页控制台设参后点启动, 支持单机/集群, 不含结果统计.

### Changed

- deploy: 统一 4 个部署脚本风格 (`die`, `need`, `load_dotenv`), 适配 macOS/Linux 双系统 (消除 `sed -i` 跨平台差异, `loadgen.sh` 采用 POSIX `nohup ... &` 后台解绑); 修复自定义 bind IP 时的本地探活与集群管理链路.
- deploy: 规范化 Docker Compose 配置, 为数据库节点注入 `ulimits: nofile` 与 `logging` 日志轮转限制; 单机与集群数据目录统一收口至本地 `.runtime/` 挂载并支持 `--purge` 级联清理; 可观测性监控栈支持按平台自适应 cAdvisor, Prometheus 升级为原生健康检查.
- deploy/docs: 全面清洗并规范化 `deploy/loadgen/README.md` 与 `deploy/observability/README.md` 的标点, 格式与技术对齐.
- loadgen: 控制台 html/css/js 从 `web/` 磁盘读取; 默认值与预设来自 `loadgen.example.toml`, 本机 `loadgen.toml` 不进仓库.
- loadgen: 探活改为 30s; 启动前拒绝不健康目标; 全节点不可达或 CLUSTERDOWN 时暂停派发, 保留 worker.
- loadgen: 改表单不影响正在跑的任务; 只有点启动才按当前表单开新任务 (已在跑则先停再起), 不做差值缩放.
- loadgen: 默认限速 3000 ops/s, 连接数 6.
- loadgen: 控制台目标改为 IP/端口拆分, cluster 只需一个 seed; 失败改为底部提示.
- 大集合 (Hash/Set) 物理布局改为版本化 subkey: header 为 `CollectionHeader { kind, count, version }`, subkey 为 `{db}\x01{user_key_len}{user_key}{version}{tag}{sub}`. 开发阶段无存量数据, 不做旧布局兼容; 父 key 可含任意字节, field/member 不再受 u16 限长约束.
- `TtlExpireFilter` 改名为 `AikvCompactionFilter`: compaction 时同时回收过期主键与无效 subkey (版本不匹配或 header 缺失), 宽限期 1 小时内的 subkey 保留. 集群模式与单机模式分别构造 (`cluster()` / `single_node()`).
- `SUNIONSTORE` / `SINTERSTORE` / `SDIFFSTORE` 目标若已存在且不是 Set, 直接覆盖 (与 Redis 一致); 结果为空时删除目标 key 并返回 0.
- `OBJECT ENCODING` 对 subkey 布局的大集合返回 `hashtable`, 内联集合仍返回 `listpack`.
- `KvStorage` 移除 `raw_subkey_*` 方法, 新增 `subkey_get_many` / `subkey_scan` / `commit_collection` / `put_value` / `get_expanded` / `move_key`; 命令层不再直接操作 subkey 物理键.

### Performance

- SET/DEL 热路径: 仅当本节点有 WATCH 时才写 watch meta, 并与用户写同一次 Raft propose; meta 使用相同 hash tag 同 slot (`#83`).
- 多 field `HSET` / 多 member `SADD` 在对象已是 subkey 布局时, 只调用一次 `commit_collection`, 不再逐 field 读写.
- `DEL` / `UNLINK` 大集合为 O(1): 只删除 header, 旧 subkey 交给 compaction 回收, 不再逐条删除.
- ZSET 读取时批量解码并避免整值克隆 (`72ee3a3`).

### Fixed

- 集群启用时拒绝 `engine=memory`, 避免 `init_cluster` panic 与绕过 Raft 的内存写入 (`#77`).
- 修复 `MULTI/EXEC` 跨 slot 部分执行与事务期间客户端命令插队问题, 并对齐 Redis 运行时错误不回滚语义 (`#78`).
- WATCH 版本改为存储层 meta key (含 DB), 随写入 apply 递增, 支持跨连接冲突检测 (`#79`).
- 修复 `DEL` 后同名 key 重建为大 Hash/Set 时旧 field/member 复活的问题; 删除集合只删 header, 旧 subkey 不再可见 (`#P1`).
- 修复大集合惰性过期或 compaction TTL 只删除 header、旧 subkey 残留的问题 (`#P2`).
- 修复 `MOVE` 对大集合只搬运 header 导致目标库数据丢失的问题 (`#P3`).
- 修复 `SWAPDB` 对大集合只交换 header 导致数据丢失与孤儿 subkey 的问题 (`#P4`).
- 修复 `DUMP` / `RESTORE` / `MIGRATE` 对大集合只序列化 header 导致数据丢失的问题 (`#P5`).
- 修复 Lua 脚本写回大集合后旧 subkey 残留并在后续转换时复活的问题 (`#P6`).
- `MemoryEngine` 不产生 subkey 布局, 大集合保持内联存储, 修复第 65 个 field 写入报错的问题 (`#P7`).
- 修复 `KEYS` / `SCAN` 把 subkey 误当作 key 返回的问题; subkey 与主键空间不再共享前缀 (`#P8`).
- 修复多 field `HSET` 逐 field 读写导致的写入延迟 (单 field 约 3 ms) (`#P9`).
- 修复 subkey 读错误被 `unwrap_or_default` 吞掉导致 `count` 漂移的问题, 读错误改为向上传播 (`#P10`).
- 修复 `HSCAN` / `SSCAN` 游标越界时 `paginate_slice` panic 的问题, 越界返回游标 0 与空页 (`#P11`).
- 父 key 含 `0x01` 不再被拒绝; field / member 长度不再受 u16 限制, 与 Redis 行为对齐 (`#P12`).
- `SMOVE` 目标与 `SUNIONSTORE` / `SINTERSTORE` / `SDIFFSTORE` 结果超过阈值时走 subkey 布局, 不再以内联大集合存储; `*STORE` 结果为空时不再写入空集合 (`#P13`).
