# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
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
  - Full integration of AiDb v0.1.0 LSM-Tree storage engine
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
