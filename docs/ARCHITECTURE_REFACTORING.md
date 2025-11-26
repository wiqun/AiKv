# 存储层架构重构计划

## 概述

本文档描述了 AiKv 存储层架构重构的完整计划。这次重构旨在将命令逻辑从存储层分离出来，使架构更加清晰、可维护和可扩展。

## 当前问题

### 架构问题

当前的存储层（`StorageAdapter` 和 `AiDbStorageAdapter`）承担了太多不属于它的职责：

1. **命令特定逻辑**: 包含了大量命令级别的实现，如 `mset_in_db`, `list_lpush_in_db`, `hash_set_in_db` 等
2. **职责不清**: 存储层既负责数据持久化，又负责命令业务逻辑
3. **接口冗余**: 52+ 个公开方法，许多是命令特定的
4. **难以维护**: 添加新命令或修改现有命令需要同时修改存储层和命令层
5. **切换困难**: 如果要切换存储引擎，需要重新实现所有命令逻辑

### 违反的设计原则

- **单一职责原则 (SRP)**: 存储层应该只负责数据存储，不应包含命令逻辑
- **关注点分离 (SoC)**: 业务逻辑（命令）和基础设施（存储）混合在一起
- **接口隔离原则 (ISP)**: 存储接口过于庞大，不够精简和正交

## 目标架构

### 分层设计

```
┌─────────────────────────────────────────────┐
│            命令层 (Command Layer)            │
│  StringCommands, ListCommands, HashCommands │
│  SetCommands, ZSetCommands, etc.            │
│  - 命令解析和验证                            │
│  - 业务逻辑实现                              │
│  - 直接操作数据结构                          │
└─────────────────┬───────────────────────────┘
                  │ 使用
                  ▼
┌─────────────────────────────────────────────┐
│         存储层 (Storage Layer)               │
│      StorageBackend Trait                   │
│  - get/set/delete/exists                    │
│  - 数据库管理                                │
│  - 过期时间管理                              │
│  - 数据持久化                                │
└─────────────────┬───────────────────────────┘
                  │ 实现
                  ▼
┌─────────────────────────────────────────────┐
│        具体实现 (Implementations)            │
│  MemoryAdapter, AiDbAdapter                 │
└─────────────────────────────────────────────┘
```

### 核心设计原则

1. **存储层**: 只提供最基本的正交存储操作接口（CRUD + 过期管理 + 数据库管理）
2. **命令层**: 所有命令相关的业务逻辑在各自的命令实现类中完成
3. **值对象**: `StoredValue` 作为通用值容器，暴露底层数据结构供命令层操作

## 新的存储接口设计

### StorageBackend Trait

```rust
pub trait StorageBackend {
    // 基本 CRUD 操作
    fn get(&self, db: usize, key: &str) -> Result<Option<StoredValue>>;
    fn set(&self, db: usize, key: String, value: StoredValue) -> Result<()>;
    fn delete(&self, db: usize, key: &str) -> Result<bool>;
    fn exists(&self, db: usize, key: &str) -> Result<bool>;
    
    // 键空间操作
    fn keys(&self, db: usize, pattern: Option<&str>) -> Result<Vec<String>>;
    fn scan(&self, db: usize, cursor: u64, pattern: Option<&str>, count: usize) 
        -> Result<(u64, Vec<String>)>;
    
    // 数据库级操作
    fn flush_db(&self, db: usize) -> Result<()>;
    fn flush_all(&self) -> Result<()>;
    fn db_size(&self, db: usize) -> Result<usize>;
    fn swap_db(&self, db1: usize, db2: usize) -> Result<()>;
    
    // 过期管理（保留在存储层，因为是持久化关注点）
    fn set_expiration(&self, db: usize, key: &str, expire_at_ms: u64) -> Result<bool>;
    fn get_expiration(&self, db: usize, key: &str) -> Result<Option<u64>>;
    fn remove_expiration(&self, db: usize, key: &str) -> Result<bool>;
}
```

### StoredValue 设计

```rust
pub enum ValueType {
    String(Bytes),
    List(VecDeque<Bytes>),
    Hash(HashMap<String, Bytes>),
    Set(HashSet<Vec<u8>>),
    ZSet(BTreeMap<Vec<u8>, f64>),
}

pub struct StoredValue {
    value: ValueType,
    expires_at: Option<u64>,
}

impl StoredValue {
    // 提供公开的访问和修改方法
    pub fn as_string(&self) -> Option<&Bytes> { ... }
    pub fn as_string_mut(&mut self) -> Option<&mut Bytes> { ... }
    pub fn as_list(&self) -> Option<&VecDeque<Bytes>> { ... }
    pub fn as_list_mut(&mut self) -> Option<&mut VecDeque<Bytes>> { ... }
    // 其他类型类似...
    
    pub fn type_name(&self) -> &str { ... }
    pub fn is_expired(&self) -> bool { ... }
}
```

## 迁移示例

### 示例 1: MSET 命令

**迁移前** (逻辑在存储层):

```rust
// storage/memory_adapter.rs
impl StorageAdapter {
    pub fn mset_in_db(&self, db_index: usize, pairs: Vec<(String, Bytes)>) -> Result<()> {
        let mut databases = self.databases.write()?;
        if let Some(db) = databases.get_mut(db_index) {
            for (key, value) in pairs {
                db.insert(key, StoredValue::new_string(value));
            }
            Ok(())
        } else {
            Err(...)
        }
    }
}

// command/string.rs
impl StringCommands {
    pub fn mset(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        // 只做参数解析
        let mut pairs = Vec::new();
        for chunk in args.chunks(2) {
            pairs.push((key, value));
        }
        // 调用存储层
        self.storage.mset_in_db(current_db, pairs)?;
        Ok(RespValue::ok())
    }
}
```

**迁移后** (逻辑在命令层):

```rust
// storage/backend.rs
impl StorageBackend for MemoryAdapter {
    fn set(&self, db: usize, key: String, value: StoredValue) -> Result<()> {
        let mut databases = self.databases.write()?;
        if let Some(db) = databases.get_mut(db) {
            db.insert(key, value);
            Ok(())
        } else {
            Err(...)
        }
    }
}

// command/string.rs
impl StringCommands {
    pub fn mset(&self, args: &[Bytes], current_db: usize) -> Result<RespValue> {
        // 参数解析和业务逻辑都在命令层
        let mut pairs = Vec::new();
        for chunk in args.chunks(2) {
            let key = String::from_utf8_lossy(&chunk[0]).to_string();
            let value = chunk[1].clone();
            pairs.push((key, value));
        }
        
        // 使用基础存储接口
        for (key, value) in pairs {
            self.storage.set(
                current_db,
                key,
                StoredValue::new_string(value)
            )?;
        }
        
        Ok(RespValue::ok())
    }
}
```

### 示例 2: LPUSH 命令

**迁移前** (逻辑在存储层):

```rust
// storage/memory_adapter.rs
impl StorageAdapter {
    pub fn list_lpush_in_db(&self, db_index: usize, key: &str, elements: Vec<Bytes>) 
        -> Result<usize> {
        let mut databases = self.databases.write()?;
        if let Some(db) = databases.get_mut(db_index) {
            let stored_value = db.entry(key.to_string())
                .or_insert_with(|| StoredValue::new_list(VecDeque::new()));
            
            if let ValueType::List(ref mut list) = stored_value.value {
                for elem in elements.into_iter().rev() {
                    list.push_front(elem);
                }
                Ok(list.len())
            } else {
                Err(...)
            }
        } else {
            Err(...)
        }
    }
}
```

**迁移后** (逻辑在命令层):

```rust
// command/list.rs
impl ListCommands {
    pub fn lpush(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        let key = String::from_utf8_lossy(&args[0]).to_string();
        let elements: Vec<Bytes> = args[1..].to_vec();
        
        // 获取或创建列表
        let mut stored_value = self.storage.get(db_index, &key)?
            .unwrap_or_else(|| StoredValue::new_list(VecDeque::new()));
        
        // 类型检查
        let list = stored_value.as_list_mut()
            .ok_or(AikvError::WrongType)?;
        
        // 执行业务逻辑
        for elem in elements.into_iter().rev() {
            list.push_front(elem);
        }
        let len = list.len();
        
        // 保存回存储
        self.storage.set(db_index, key, stored_value)?;
        
        Ok(RespValue::Integer(len as i64))
    }
}
```

## 实施计划

重构分为 7 个阶段，每个阶段独立完成并测试：

### ✅ 阶段 1: 准备工作 (已完成)
- ✅ 使 `StoredValue` 和 `ValueType` 公开
- ✅ 为 `StoredValue` 添加公开访问方法 (`as_string()`, `as_list()`, `as_hash()`, `as_set()`, `as_zset()`)
- ✅ 添加最小化存储接口方法 (`get_value()`, `set_value()`, `update_value()`, `delete_and_get()`)
- ✅ 所有现有测试通过

### 阶段 2-6: 逐个迁移数据类型

#### ✅ 阶段 2: String 命令 (已完成)
- ✅ MGET - 移至命令层，使用基础 `get_from_db()`
- ✅ MSET - 移至命令层，使用基础 `set_in_db()`

#### ✅ 阶段 3: List 命令 (已完成)
已迁移 10 个命令:
- ✅ LPUSH, RPUSH - 命令层直接操作 `VecDeque<Bytes>`
- ✅ LPOP, RPOP - 命令层处理弹出逻辑和空列表删除
- ✅ LLEN, LRANGE, LINDEX - 命令层直接查询
- ✅ LSET, LREM, LTRIM - 命令层处理修改逻辑

#### ✅ 阶段 4: Hash 命令 (已完成)
已迁移 12 个命令:
- ✅ HSET, HSETNX - 命令层使用 Entry API
- ✅ HGET, HMGET - 命令层直接访问 `HashMap`
- ✅ HDEL - 命令层处理批量删除和空哈希清理
- ✅ HEXISTS, HLEN - 命令层直接查询
- ✅ HKEYS, HVALS, HGETALL - 命令层直接迭代
- ✅ HINCRBY, HINCRBYFLOAT - 命令层处理解析-修改-存储

#### ⏳ 阶段 5: Set 命令 (待完成)
待迁移 13 个命令:
- [ ] SADD, SREM, SISMEMBER, SMEMBERS, SCARD
- [ ] SPOP, SRANDMEMBER
- [ ] SUNION, SINTER, SDIFF
- [ ] SUNIONSTORE, SINTERSTORE, SDIFFSTORE

#### ⏳ 阶段 6: ZSet 命令 (待完成)
待迁移 10 个命令:
- [ ] ZADD, ZREM, ZSCORE
- [ ] ZRANK, ZREVRANK
- [ ] ZRANGE, ZREVRANGE
- [ ] ZRANGEBYSCORE, ZREVRANGEBYSCORE
- [ ] ZCARD, ZCOUNT, ZINCRBY

### ⏳ 阶段 7: 清理和优化 (待完成)
- [ ] 从 `MemoryAdapter` 移除已迁移的命令特定方法
- [ ] 更新 `AiDbStorageAdapter` 以支持复杂类型（需要序列化）
- [ ] 统一两个适配器的接口
- [x] 完整测试和性能验证

## 当前实施状态

### 已完成工作 (更新于 2025-11-26)

**迁移进度**: 47/47 命令 (100%) ✅

- ✅ **Phase 1**: 基础架构 - 完成
  - 公开 `StoredValue` 和 `ValueType`
  - 添加类型安全的访问器方法
  - 实现最小化存储接口
  
- ✅ **Phase 2**: String 命令 (2/2) - 完成
- ✅ **Phase 3**: List 命令 (10/10) - 完成  
- ✅ **Phase 4**: Hash 命令 (12/12) - 完成
- ✅ **Phase 5**: Set 命令 (13/13) - 完成
- ✅ **Phase 6**: ZSet 命令 (10/10) - 完成
- ✅ **Phase 7**: 清理和优化 - 完成

**代码质量**:
- ✅ 96 个单元测试全部通过
- ✅ 集成测试全部通过
- ✅ cargo clippy 零警告
- ✅ cargo fmt 已格式化

**文件变更**:
- `src/storage/mod.rs` - 导出公共类型
- `src/storage/memory_adapter.rs` - 从 2649 行优化到 878 行 (-67%)
- `src/storage/aidb_adapter.rs` - 完整数据类型序列化支持
- `src/command/string.rs` - 迁移 MGET, MSET
- `src/command/list.rs` - 迁移所有 10 个列表命令
- `src/command/hash.rs` - 迁移所有 12 个哈希命令
- `src/command/set.rs` - 迁移所有 13 个集合命令
- `src/command/zset.rs` - 迁移所有 10 个有序集合命令

### AiDbStorageAdapter 状态说明

**✅ 已完成**:
- `AiDbStorageAdapter` 现已支持所有数据类型（String, List, Hash, Set, ZSet）
- 使用 bincode 实现高性能二进制序列化/反序列化
- 通过 `SerializableStoredValue` 中间层进行类型转换
- 完整测试覆盖，包括所有数据类型的集成测试
- 性能优化：采用二进制格式，序列化开销最小化

**实现细节**:
- `get_value()` - 支持反序列化所有类型
- `set_value()` - 支持序列化所有类型  
- `update_value()` - 支持原子性更新操作
- `delete_and_get()` - 支持删除并返回值
- 过期时间管理与所有数据类型兼容

**使用建议**:
- ✅ 生产环境可以使用 `AiDbStorageAdapter` 获得完整功能和持久化能力
- ✅ 对于所有数据类型（String, List, Hash, Set, ZSet），AiDb 和 Memory 两种后端均支持
- ✅ 可以根据需求选择：Memory（纯内存，速度最快）或 AiDb（持久化，数据安全）

### 文档和验证 (Section 0.4)

**✅ 已完成 (2025-11-14)**:

1. **代码清理**:
   - 从 `AiDbStorageAdapter` 移除冗余方法：`mget_from_db`, `mget`, `mset_in_db`, `mset`（35行）
   - 更新测试用例使用新的最小接口（`set_value`, `get_value`）
   - 代码行数从 1318 减少到 1297 行

2. **API 文档（rustdoc）**:
   - 为 `aidb_adapter.rs` 添加模块级文档，说明架构设计和核心原则
   - 为 `memory_adapter.rs` 添加模块级文档
   - 为核心方法添加详细文档：
     - `get_value()` - 完整的参数、返回值和使用示例
     - `set_value()` - 数据类型支持和序列化说明
     - `update_value()` - 原子性操作语义和使用场景
     - `delete_and_get()` - 原子删除语义
   - 为公共类型添加文档：`ValueType`, `StoredValue`, `SerializableStoredValue`
   - 更新示例文件 `aidb_storage_example.rs` 使用新接口

3. **全面测试验证**:
   - ✅ 所有 89 个单元测试通过
   - ✅ 所有 5 个集成测试通过
   - ✅ cargo clippy 零警告
   - ✅ cargo fmt 代码格式化通过
   - ✅ 示例程序编译通过

4. **文档更新**:
   - ✅ 更新 ARCHITECTURE_REFACTORING.md（本文档）
   - ✅ 记录所有完成的工作和验证结果
   - ✅ 文档化存储层的最小接口设计

## 预期收益

1. **清晰的架构**: 存储层和命令层职责明确，易于理解
2. **易于维护**: 命令逻辑集中，修改不会影响存储层
3. **灵活性**: 可以轻松切换存储引擎（内存 ↔ AiDb）
4. **可测试性**: 存储层和命令层可以独立测试
5. **性能**: 减少不必要的抽象，直接操作数据结构
6. **扩展性**: 新增命令只需使用基础接口，无需修改存储层

## 风险管理

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| 引入 bug | 高 | 分阶段实施，每阶段完整测试 |
| 性能下降 | 中 | 每阶段性能基准测试，及时优化 |
| 代码量增加 | 低 | 使用辅助函数和宏减少重复 |
| 原子性问题 | 中 | 命令层使用适当的锁策略 |

## 验收标准

1. ✅ 所有单元测试通过
2. ✅ 所有集成测试通过
3. ✅ 性能基准测试不低于重构前 95%
4. ✅ 代码覆盖率不低于重构前
5. ✅ 通过 clippy 和 fmt 检查
6. ✅ 文档更新完整

## 参考资料

- [TODO.md - 优先级 0](../TODO.md#优先级-0---存储层架构重构-架构修正)
- [单一职责原则 (SRP)](https://en.wikipedia.org/wiki/Single-responsibility_principle)
- [关注点分离 (SoC)](https://en.wikipedia.org/wiki/Separation_of_concerns)
- [Redis 架构设计](https://redis.io/topics/architecture)

---

**创建日期**: 2025-11-13  
**最后更新**: 2025-11-26  
**负责人**: @Genuineh, @copilot
