# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **AiDb v0.5.0 Upgrade (2025-12-11)**
  - Upgraded AiDb dependency from v0.4.1 to v0.5.0
  - AiDb v0.5.0 improvements:
    - Added `rmp-serde` dependency for improved MessagePack serialization in raft-cluster
    - Enhanced Raft consensus serialization performance
    - Stability improvements for production environments
  - Updated all documentation references to v0.5.0
  - All 118 library tests pass
  - All 93 cluster tests pass
  - Backward compatible - no API changes required

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
  - 新增完整的命令元数据表（100+ 命令），支持 COMMAND 系列命令
  - Server 命令从 9 个增加到 16 个
  - 新增 4 个单元测试验证新命令功能

- **Documentation Review and Organization (2025-11-26)**
  - Updated TODO.md with comprehensive current status
  - Updated SUMMARY.md with full feature list and command count
  - Updated DEVELOPMENT_PLAN.md with current implementation state
  - Synchronized all documentation with actual project state
  - Updated test count references (96 unit tests)
  - Updated AiDb version references to v0.4.1

- **AiDb v0.4.1 Multi-Raft Upgrade (2025-11-25)**
  - Upgraded AiDb dependency from v0.1.0 to v0.4.1
  - AiDb v0.4.1 brings Multi-Raft + Sharding architecture support:
    - Thin Replication for 90%+ replication cost reduction
    - MetaRaft for global slot mapping and node state management
    - MultiRaftNode implementation for distributed consensus
    - CRC16-based slot calculation and per-group AiDb instances
    - Dynamic member management for Multi-Raft
    - Online slot migration with batch optimization and dual-write
    - Production readiness: configuration, metrics, snapshots, log cleanup
  - Added comprehensive 18-week roadmap for AiKv v1.0.0 (target: 2026.03.31)
  - Updated TODO.md with detailed milestones and performance targets
  - Backward compatible API - all existing tests pass

- **AiKv v1.0.0 Roadmap**
  - Stage 0 (Week 1-2): Multi-Raft integration
  - Stage 1 (Week 3-4): 16384 slot mapping and routing
  - Stage 2 (Week 5-6): CLUSTER commands and node management
  - Stage 3 (Week 7-9): Online slot migration (reshard)
  - Stage 4 (Week 10-12): Replication and auto failover
  - Stage 5 (Week 13-15): Advanced data types + Lua + Pub/Sub
  - Stage 6 (Week 16-17): Stress testing and official test suite
  - Stage 7 (Week 18): v1.0.0 release with Docker, Helm, Prometheus

- **AiDbStorageAdapter Complete Data Type Support (2025-11-13)**
  - Full serialization support for all data types (String, List, Hash, Set, ZSet)
  - Using bincode for high-performance binary serialization/deserialization
  - New `SerializableStoredValue` intermediate representation for efficient storage
  - Core methods: `get_value()`, `set_value()`, `update_value()`, `delete_and_get()`
  - Feature parity with MemoryAdapter achieved
  - 11 new comprehensive test cases covering all data types
  - Cross-database operations and expiration handling
  - Zero performance overhead - optimized for production use
  - No backward compatibility required - clean implementation

- **Storage Layer Architecture Refactoring (Phase 1-4)**
  - New minimal storage interface with `get_value()`, `set_value()`, `update_value()`, `delete_and_get()`
  - Public `StoredValue` and `ValueType` with typed accessor methods
  - Type-safe accessors: `as_string()`, `as_list()`, `as_hash()`, `as_set()`, `as_zset()`
  - Mutable accessors: `as_list_mut()`, `as_hash_mut()`, `as_set_mut()`, `as_zset_mut()`
  - Documentation: `docs/ARCHITECTURE_REFACTORING.md` - complete refactoring plan and status

- **AiDb Storage Engine Integration**
  - Full integration of AiDb v0.4.0 LSM-Tree storage engine with Multi-Raft support
  - New `AiDbStorageAdapter` with persistent storage support
  - Support for WAL (Write-Ahead Log) and SSTable persistence
  - Bloom Filter for faster key lookups
  - Snappy compression support
  - Multi-database support (16 databases)
  - All Redis storage operations:
    - CRUD operations (get, set, delete, exists)
    - Expiration management (TTL, EXPIRE, PERSIST)
    - Batch operations (MGET, MSET)
    - Key operations (RENAME, COPY, MOVE)
    - Database operations (FLUSHDB, FLUSHALL, DBSIZE)
- Dual storage backend support
  - Memory-based storage (original, fast, non-persistent)
  - AiDb-based storage (persistent, durable, with LSM-Tree)
- Example code for AiDb storage usage (`examples/aidb_storage_example.rs`)
- Comprehensive tests for AiDb adapter (5 new test cases)
- Updated documentation to reflect AiDb integration
- GitHub Actions CI/CD workflows
  - Continuous Integration pipeline (`.github/workflows/ci.yml`)
  - Security audit workflow (`.github/workflows/security.yml`)
  - Release workflow (`.github/workflows/release.yml`)
- Code formatting and linting configuration
  - `rustfmt.toml` for consistent code formatting
  - `clippy.toml` for code quality checks
  - `.editorconfig` for editor consistency
- Development documentation
  - `TODO.md` - comprehensive task list for future development
  - `CONTRIBUTING.md` - contribution guidelines with code standards
  - `Makefile` - common development tasks
  - `deny.toml` - cargo-deny configuration
- TODO list with prioritized tasks
  - RESP3 protocol support plan
  - Redis DB and Key commands plan
  - Performance optimization plan
  - Cluster and high availability plan

### Changed
- **Storage Layer Architecture Refactoring (24/52 commands migrated - 46%)**
  - Migrated String commands (2/2): MGET, MSET
    - Commands now use basic `get_from_db()`/`set_in_db()` instead of specialized batch methods
  - Migrated List commands (10/10): LPUSH, RPUSH, LPOP, RPOP, LLEN, LRANGE, LINDEX, LSET, LREM, LTRIM
    - Commands directly manipulate `VecDeque<Bytes>` using `get_value()`/`set_value()`
    - Business logic (index normalization, range extraction, etc.) moved from storage to command layer
  - Migrated Hash commands (12/12): HSET, HSETNX, HGET, HMGET, HDEL, HEXISTS, HLEN, HKEYS, HVALS, HGETALL, HINCRBY, HINCRBYFLOAT
    - Commands directly manipulate `HashMap<String, Bytes>`
    - Increment operations (HINCRBY, HINCRBYFLOAT) now parse-modify-store in command layer
    - Used Entry API for HSETNX to avoid clippy warnings
  - Set and ZSet commands remain to be migrated in future phases

- Renamed `aidb_adapter.rs` to `memory_adapter.rs` for clarity
- Created new `aidb_adapter.rs` with real AiDb integration
- Updated `storage/mod.rs` to export both storage adapters and new public types
- Updated `Cargo.toml` to include AiDb dependency
- Added `tempfile` as dev-dependency for testing
- Updated README.md with storage engine information
- Updated TODO.md to mark AiDb integration tasks as complete
- Updated project goals to include RESP3 and DB/Key commands
- Improved code formatting across all files
- Updated Cargo edition from 2024 to 2021 (stable)

### Notes
- **AiDbStorageAdapter Limitation**: Currently only supports string values (raw Bytes). Complex types (List, Hash, Set, ZSet) require serialization support to be added in future releases.
- **Migration Status**: Memory adapter fully refactored for String, List, and Hash commands. Remaining work: Set (13 commands), ZSet (10 commands), and cleanup phase.

## [0.1.0] - 2025-11-11

### Added
- Initial implementation of Redis protocol compatibility layer
- RESP2 protocol parser and serializer
  - Support for all 5 RESP types (Simple String, Error, Integer, Bulk String, Array)
  - Complete serialization and deserialization
- String commands (8 commands)
  - GET, SET (with EX, NX, XX options)
  - DEL, EXISTS
  - MGET, MSET
  - STRLEN, APPEND
- JSON commands (7 commands)
  - JSON.GET, JSON.SET (with NX, XX options)
  - JSON.DEL, JSON.TYPE
  - JSON.STRLEN, JSON.ARRLEN, JSON.OBJLEN
  - Simplified JSONPath support
- Storage adapter with thread-safe in-memory implementation
- Async TCP server using Tokio
- Comprehensive test suite (28 unit tests)
- Complete Chinese documentation
  - Development plan (`docs/DEVELOPMENT_PLAN.md`)
  - API documentation (`docs/API.md`)
  - Deployment guide (`docs/DEPLOYMENT.md`)
  - Project summary (`docs/SUMMARY.md`)
- Example client code (`examples/client_example.rs`)

### Technical Details
- Rust 2021 edition
- Tokio async runtime for high-performance networking
- Thread-safe storage operations using RwLock
- Modular architecture with clear separation of concerns
- Production-ready error handling

[Unreleased]: https://github.com/Genuineh/AiKv/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Genuineh/AiKv/releases/tag/v0.1.0
