# 存储层架构对比

## 当前架构（问题）

```
┌───────────────────────────────────────────────────────────────┐
│                      命令层 (Commands)                         │
│                                                                │
│  StringCommands  ListCommands  HashCommands  SetCommands      │
│      ↓               ↓              ↓              ↓          │
│   简单调用        简单调用       简单调用       简单调用       │
└───────────────────────┬───────────────────────────────────────┘
                        │
                        ▼
┌───────────────────────────────────────────────────────────────┐
│                  存储层 (Storage Layer)                        │
│                    StorageAdapter                              │
│                                                                │
│  基础操作 (4个):                                               │
│    • get_from_db                                              │
│    • set_in_db                                                │
│    • delete_from_db                                           │
│    • exists_in_db                                             │
│                                                                │
│  命令特定操作 (52+个) ❌ 问题所在:                            │
│    String: mset_in_db, mget_from_db                           │
│    List: list_lpush_in_db, list_rpush_in_db,                 │
│          list_lpop_in_db, list_rpop_in_db, ...               │
│    Hash: hash_set_in_db, hash_get_in_db,                     │
│          hash_mget_in_db, hash_del_in_db, ...                │
│    Set:  set_add_in_db, set_rem_in_db,                       │
│          set_union_in_db, set_inter_in_db, ...               │
│    ZSet: zset_add_in_db, zset_rem_in_db,                     │
│          zset_range_in_db, zset_score_in_db, ...             │
│                                                                │
│  ⚠️ 问题:                                                      │
│    - 命令逻辑和存储逻辑混合                                    │
│    - 违反单一职责原则                                          │
│    - 难以切换存储引擎                                          │
│    - 接口不够正交和精简                                        │
└───────────────────────────────────────────────────────────────┘
```

## 新架构（目标）

```
┌───────────────────────────────────────────────────────────────┐
│                      命令层 (Commands)                         │
│                                                                │
│  StringCommands  ListCommands  HashCommands  SetCommands      │
│                                                                │
│  ✅ 包含所有命令业务逻辑:                                      │
│    • 参数解析和验证                                            │
│    • 数据类型检查                                              │
│    • 业务规则实现                                              │
│    • 直接操作数据结构 (VecDeque, HashMap, HashSet, BTreeMap) │
│                                                                │
│  示例: MSET                                                    │
│    1. 解析 key-value pairs                                    │
│    2. for (key, value) in pairs:                              │
│         storage.set(db, key, StoredValue::String(value))      │
│                                                                │
│  示例: LPUSH                                                   │
│    1. value = storage.get(db, key)                            │
│    2. list = value.as_list_mut()                              │
│    3. for elem in elements: list.push_front(elem)             │
│    4. storage.set(db, key, value)                             │
└───────────────────────┬───────────────────────────────────────┘
                        │ 使用简洁的接口
                        ▼
┌───────────────────────────────────────────────────────────────┐
│                  存储层 (Storage Layer)                        │
│                   StorageBackend Trait                         │
│                                                                │
│  ✅ 只包含基础存储操作 (~15个方法):                           │
│                                                                │
│  基本 CRUD:                                                    │
│    • get(db, key) -> Option<StoredValue>                      │
│    • set(db, key, value: StoredValue)                         │
│    • delete(db, key) -> bool                                  │
│    • exists(db, key) -> bool                                  │
│                                                                │
│  键空间操作:                                                   │
│    • keys(db, pattern) -> Vec<String>                         │
│    • scan(db, cursor, pattern, count) -> (cursor, Vec<String>)│
│                                                                │
│  数据库级操作:                                                 │
│    • flush_db(db)                                             │
│    • flush_all()                                              │
│    • db_size(db) -> usize                                     │
│    • swap_db(db1, db2)                                        │
│                                                                │
│  过期管理:                                                     │
│    • set_expiration(db, key, expire_at_ms)                    │
│    • get_expiration(db, key) -> Option<u64>                   │
│    • remove_expiration(db, key)                               │
│                                                                │
│  ✅ 优势:                                                      │
│    - 接口精简、正交                                            │
│    - 职责单一、专注存储                                        │
│    - 易于实现不同的存储引擎                                    │
│    - 易于测试和维护                                            │
└───────────────────────┬───────────────────────────────────────┘
                        │ 实现
                        ▼
┌───────────────────────────────────────────────────────────────┐
│                  存储引擎实现                                   │
│                                                                │
│    MemoryAdapter           AiDbAdapter                        │
│    (内存实现)              (持久化实现)                        │
│                                                                │
│    两个实现都使用相同的 StorageBackend trait                   │
│    可以轻松切换，无需修改命令层代码                            │
└───────────────────────────────────────────────────────────────┘
```

## StoredValue 结构

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
    // 公开访问方法，供命令层使用
    pub fn as_string(&self) -> Option<&Bytes>;
    pub fn as_string_mut(&mut self) -> Option<&mut Bytes>;
    pub fn as_list(&self) -> Option<&VecDeque<Bytes>>;
    pub fn as_list_mut(&mut self) -> Option<&mut VecDeque<Bytes>>;
    pub fn as_hash(&self) -> Option<&HashMap<String, Bytes>>;
    pub fn as_hash_mut(&mut self) -> Option<&mut HashMap<String, Bytes>>;
    pub fn as_set(&self) -> Option<&HashSet<Vec<u8>>>;
    pub fn as_set_mut(&mut self) -> Option<&mut HashSet<Vec<u8>>>;
    pub fn as_zset(&self) -> Option<&BTreeMap<Vec<u8>, f64>>;
    pub fn as_zset_mut(&mut self) -> Option<&mut BTreeMap<Vec<u8>, f64>>;
    
    pub fn type_name(&self) -> &str;
    pub fn is_expired(&self) -> bool;
}
```

## 迁移对比示例

### MSET 命令

**迁移前 (逻辑在存储层):**
```rust
// command/string.rs
impl StringCommands {
    pub fn mset(&self, args: &[Bytes], db: usize) -> Result<RespValue> {
        let mut pairs = Vec::new();
        for chunk in args.chunks(2) {
            pairs.push((key, value)); // 简单解析
        }
        self.storage.mset_in_db(db, pairs)?; // ❌ 调用存储层特定方法
        Ok(RespValue::ok())
    }
}

// storage/memory_adapter.rs
impl StorageAdapter {
    pub fn mset_in_db(&self, db: usize, pairs: Vec<(String, Bytes)>) -> Result<()> {
        let mut databases = self.databases.write()?;
        // ❌ 命令逻辑在存储层
        for (key, value) in pairs {
            db.insert(key, StoredValue::new_string(value));
        }
        Ok(())
    }
}
```

**迁移后 (逻辑在命令层):**
```rust
// command/string.rs
impl StringCommands {
    pub fn mset(&self, args: &[Bytes], db: usize) -> Result<RespValue> {
        let mut pairs = Vec::new();
        for chunk in args.chunks(2) {
            let key = String::from_utf8_lossy(&chunk[0]).to_string();
            let value = chunk[1].clone();
            pairs.push((key, value));
        }
        
        // ✅ 命令逻辑在命令层
        for (key, value) in pairs {
            self.storage.set(db, key, StoredValue::new_string(value))?;
        }
        
        Ok(RespValue::ok())
    }
}

// storage/backend.rs
pub trait StorageBackend {
    // ✅ 只有基础 set 方法，不需要 mset_in_db
    fn set(&self, db: usize, key: String, value: StoredValue) -> Result<()>;
}
```

### LPUSH 命令

**迁移前 (逻辑在存储层):**
```rust
// storage/memory_adapter.rs - 40+ 行复杂逻辑
pub fn list_lpush_in_db(&self, db: usize, key: &str, elements: Vec<Bytes>) 
    -> Result<usize> {
    let mut databases = self.databases.write()?;
    let stored_value = db.entry(key.to_string())
        .or_insert_with(|| StoredValue::new_list(VecDeque::new()));
    
    if let ValueType::List(ref mut list) = stored_value.value {
        for elem in elements.into_iter().rev() {
            list.push_front(elem); // ❌ 命令逻辑在存储层
        }
        Ok(list.len())
    } else {
        Err(AikvError::WrongType)
    }
}
```

**迁移后 (逻辑在命令层):**
```rust
// command/list.rs - 逻辑清晰，在正确的地方
pub fn lpush(&self, args: &[Bytes], db: usize) -> Result<RespValue> {
    let key = String::from_utf8_lossy(&args[0]).to_string();
    let elements: Vec<Bytes> = args[1..].to_vec();
    
    // ✅ 使用基础存储接口
    let mut value = self.storage.get(db, &key)?
        .unwrap_or_else(|| StoredValue::new_list(VecDeque::new()));
    
    // ✅ 命令逻辑在命令层
    let list = value.as_list_mut().ok_or(AikvError::WrongType)?;
    for elem in elements.into_iter().rev() {
        list.push_front(elem);
    }
    let len = list.len();
    
    self.storage.set(db, key, value)?;
    Ok(RespValue::Integer(len as i64))
}
```

## 收益总结

| 方面 | 当前架构 | 新架构 | 改进 |
|------|---------|--------|------|
| 存储层方法数 | 52+ | ~15 | ✅ 减少 70% |
| 职责分离 | ❌ 混合 | ✅ 清晰 | ✅ 符合 SRP |
| 切换存储引擎 | ❌ 困难 | ✅ 简单 | ✅ 只需实现 trait |
| 可测试性 | ⚠️ 中等 | ✅ 优秀 | ✅ 独立测试 |
| 可维护性 | ⚠️ 中等 | ✅ 优秀 | ✅ 逻辑集中 |
| 代码重复 | ⚠️ 两个适配器 | ✅ 最小化 | ✅ 共享 trait |
| 扩展性 | ⚠️ 需改存储层 | ✅ 只改命令层 | ✅ 开闭原则 |

---

**文档日期**: 2025-11-13  
**参考文档**: docs/ARCHITECTURE_REFACTORING.md, TODO.md
