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

### 阶段 1: 准备工作
- 创建新的 `StorageBackend` trait
- 为 `StoredValue` 添加公开访问方法
- 确保所有现有测试通过

### 阶段 2-6: 逐个迁移数据类型
- 阶段 2: String 命令 (MSET, MGET)
- 阶段 3: List 命令 (10 个方法)
- 阶段 4: Hash 命令 (12 个方法)
- 阶段 5: Set 命令 (13 个方法)
- 阶段 6: ZSet 命令 (10 个方法)

### 阶段 7: 清理和优化
- 移除所有命令特定方法
- 统一两个适配器的接口
- 完整测试和性能验证

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
**最后更新**: 2025-11-13  
**负责人**: @Genuineh, @copilot
