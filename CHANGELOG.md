# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **AiDb v0.6.3 Upgrade (2026-01-09)**
  - Upgraded AiDb dependency from v0.6.2 to v0.6.3
  - Fix: MemTable tombstone visibility and `DB::get()` behavior — tombstones in MemTable now block older SSTable values, resolving the issue where `DEL` returned success but `EXISTS` still returned true.
  - Verification: Rebuilt cluster and ran TL.Redis test suite locally — all 63 tests passed (previously 5 failures related to this issue).

- **AiDb v0.5.1 Upgrade (2025-12-11)**
  - Upgraded AiDb dependency from v0.5.0 to v0.5.1
  - Refactored cluster implementation to use AiDb v0.5.1's official Multi-Raft API
  - Adopted legacy compatibility layer during migration
  - All 211 tests pass (118 library + 93 cluster)
  - Exported AiDb v0.5.1 new APIs: ClusterMeta, MigrationManager, MembershipCoordinator, etc.
  - Created minimalist implementation prototypes for future optimization (~84% code reduction potential)
  - Zero-downtime upgrade with backward compatibility

### Added
- **P2: Server 命令补全 (2025-12-01)**
  - `COMMAND` - 获取所有命令的详细信息（名称、参数数量、标志、键位置等）
  - `COMMAND COUNT` - 获取支持的命令总数
  - `COMMAND INFO` - 获取指定命令的详细信息
  - `COMMAND DOCS` - 获取命令文档
  - `COMMAND GETKEYS` - 从完整命令中提取键名
  - `COMMAND HELP` - 显示帮助信息
  - `CONFIG REWRITE` - 重写配置文件（存根实现）
  - `SAVE` - 同步保存数据到磁盘
  - `BGSAVE` - 异步保存数据到磁盘
  - `LASTSAVE` - 获取上次成功保存的 Unix 时间戳
  - `SHUTDOWN` - 请求关闭服务器（支持 NOSAVE/SAVE/NOW/FORCE/ABORT 选项）
  - Server 命令从 9 个增加到 16 个
  - 新增 4 个单元测试验证新命令功能

... (rest of CHANGELOG unchanged)
