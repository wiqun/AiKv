# AiDb MultiRaft API Reference for Redis Cluster Protocol Adaptation

**目的**: 为 AiKv 开发者提供完整的 API 参考文档，帮助实现 Redis Cluster 协议胶水层。

**AiDb 版本**: v0.5.2  
**创建时间**: 2025-12-11  
**更新时间**: 2025-12-12

---

## 📋 目录

1. [概述](#概述)
2. [启用集群功能](#启用集群功能)
3. [API 组件导入](#api-组件导入)
4. [Redis Cluster 命令映射](#redis-cluster-命令映射)
5. [API 详细说明](#api-详细说明)
6. [使用示例](#使用示例)

---

## 📖 概述

AiDb v0.5.2 的 MultiRaft 实现已经完整并生产就绪，所有必要的 API 都已通过 `aidb::cluster` 模块导出。AiKv 可以直接组合使用这些 API 来实现 Redis Cluster 协议的适配。

### 实现状态 ✅

| 组件 | 状态 | 测试 | 代码行数 |
|------|------|------|---------|
| MetaRaft | ✅ 完成 | 30+ | 800+ |
| MultiRaftNode | ✅ 完成 | 30+ | 780+ |
| Router | ✅ 完成 | 15+ | 300+ |
| ShardedStateMachine | ✅ 完成 | 20+ | 400+ |
| MigrationManager | ✅ 完成 | 25+ | 800+ |
| MembershipCoordinator | ✅ 完成 | 10+ | 200+ |
| **总计** | **✅ 100%** | **144+** | **4,500+** |

### 设计理念

- **组件化** ✅: 每个功能由独立组件提供，AiKv 按需组合
- **最小化开发** ✅: AiKv 只需实现 Redis 协议格式转换，核心逻辑由 AiDb 提供
- **完整性** ✅: 所有 Redis Cluster 协议所需的底层功能都已实现
- **生产级** ✅: 完整的错误处理、监控指标、测试覆盖

### 代码量对比

| 方案 | 估算代码量 |
|------|-----------|
| AiKv 胶水层 (使用 AiDb API) | ~1000 行 |
| 从零实现 MultiRaft + 迁移 + 成员管理 | ~10000+ 行 |

---

## 🔧 启用集群功能

在 `Cargo.toml` 中添加 feature:

```toml
[features]
default = []
cluster = ["aidb/raft-cluster"]

[dependencies]
aidb = { git = "https://github.com/Genuineh/AiDb", tag = "v0.5.2" }
```

使用时启用 feature:

```bash
cargo build --features cluster
```

---

## 🔧 API 组件导入

启用 `cluster` feature 后，通过 `aidb::cluster` 导入所有组件（所有组件均已生产就绪 ✅）：

```rust
#[cfg(feature = "cluster")]
use aidb::cluster::{
    // 核心节点管理 ✅
    MultiRaftNode,        // 多 Raft Group 节点管理 (multi_raft_node.rs)
    MetaRaftNode,         // 集群元数据 Raft 管理 (meta_raft_node.rs)
    
    // 路由和分片 ✅
    Router,               // key→slot→group 路由器 (router.rs)
    SLOT_COUNT,           // slot 总数常量 (16384)
    ShardedStateMachine,  // 分片状态机 (sharded_state_machine.rs)
    
    // 迁移管理 ✅
    MigrationManager,     // 在线 slot 迁移 (slot_migration.rs)
    MigrationConfig,      // 迁移配置
    
    // 成员管理 ✅
    MembershipCoordinator, // 成员变更协调 (membership_coordinator.rs)
    ReplicaAllocator,      // 副本分配算法 (replica_allocator.rs)
    
    // 数据结构 ✅
    ClusterMeta,          // 集群元数据 (meta_types.rs)
    GroupMeta,            // Raft Group 元数据
    MetaNodeInfo,         // 节点信息 (含状态和地址)
    NodeStatus,           // 节点状态枚举
    SlotMigration,        // 迁移状态追踪
    SlotMigrationState,   // 迁移状态枚举
    
    // 存储和网络 ✅
    ShardedRaftStorage,   // 分片存储 (sharded_storage.rs)
    MultiRaftNetworkFactory, // Multi-Raft 网络工厂
    
    // Thin Replication ✅
    ThinWriteBatch,       // 薄复制批量写 (thin_replication.rs)
    ThinWriteOp,          // 薄复制操作
    
    // 类型别名
    NodeId,               // 节点 ID 类型 (u64)
    GroupId,              // Group ID 类型 (u64)
};
```

---

## 🗺️ Redis Cluster 命令映射

### 集群信息命令 ✅

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `CLUSTER INFO` | `meta_raft.get_cluster_meta()` | ✅ | 返回 `ClusterMeta`，解析字段获取集群状态 |
| `CLUSTER NODES` | `meta_raft.get_cluster_meta().nodes` | ✅ | 返回 `HashMap<NodeId, MetaNodeInfo>` |
| `CLUSTER SLOTS` | `meta_raft.get_cluster_meta().slots` + `.groups` | ✅ | 组合 slots 数组和 groups 映射 |
| `CLUSTER MYID` | `multi_raft_node.node_id()` | ✅ | 返回当前节点 ID |
| `CLUSTER KEYSLOT key` | `Router::key_to_slot(key)` | ✅ | 使用 CRC16/XMODEM 算法计算 slot |

### 节点管理命令 ✅

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `CLUSTER MEET ip port [node-id]` | `meta_raft.add_node(node_id, addr)` | ✅ | 添加新节点到集群。**同步等待** Raft 共识完成（超时 5 秒）。可选的 node-id 参数确保使用节点的实际 ID |
| `CLUSTER FORGET node_id` | `meta_raft.remove_node(node_id)` | ✅ | 从集群移除节点。**同步等待** Raft 共识完成（超时 5 秒） |

### MetaRaft 成员管理命令 ✅ (新增)

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `CLUSTER METARAFT ADDLEARNER node_id addr` | `meta_raft.add_learner(node_id, BasicNode{addr})` | ✅ | 添加节点为 MetaRaft learner。Learner 接收日志但不参与投票 |
| `CLUSTER METARAFT PROMOTE node_id [...]` | `meta_raft.change_membership(voters, true)` | ✅ | 将 learner 提升为 voter。需提供完整的 voter 列表 |
| `CLUSTER METARAFT MEMBERS` | `meta_raft.raft().metrics()` | ✅ | 列出所有 MetaRaft 成员及其角色（voter/learner） |

### Slot 管理命令 ✅

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `CLUSTER ADDSLOTS slot...` | `meta_raft.update_slots(start, end, group_id)` | ✅ | 分配 slot 范围到 group |
| `CLUSTER DELSLOTS slot...` | `meta_raft.update_slots(start, end, 0)` | ✅ | 将 slot 标记为未分配 |
| `CLUSTER SETSLOT slot NODE` | `meta_raft.update_slots(slot, slot+1, group_id)` | ✅ | 分配单个 slot |
| `CLUSTER SETSLOT MIGRATING` | `migration_manager.start_migration(slot, from, to)` | ✅ | 开始 slot 迁移 |
| `CLUSTER SETSLOT IMPORTING` | 迁移自动处理 | ✅ | 由 MigrationManager 内部管理 |
| `CLUSTER GETKEYSINSLOT` | `state_machine.scan_slot_keys_sync(group, slot)` | ✅ | 扫描 slot 中的 keys |

### 成员管理命令 ✅

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `CLUSTER REPLICATE` | `membership_coordinator.add_learner()` | ✅ | 添加为 learner 后提升为 voter |
| `CLUSTER FAILOVER` | openraft 自动故障切换 | ✅ | Raft 自动触发选举 |

### 数据操作命令 ✅

| Redis 命令 | AiDb API | 实现状态 | 说明 |
|-----------|----------|---------|------|
| `SET key value` | `multi_raft_node.put(key, value)` | ✅ | 自动路由写入 |
| `GET key` | `multi_raft_node.get(key)` | ✅ | 自动路由读取 |
| `DEL key` | `multi_raft_node.delete(key)` | ✅ | 自动路由删除 |

---

## 📚 API 详细说明

### 1. Router - 路由器

Router 负责 key 到 slot 的计算，以及 slot 到 Raft Group 的映射。

```rust
use aidb::cluster::{Router, SLOT_COUNT};

// 计算 key 对应的 slot (与 Redis 兼容的 CRC16/XMODEM 算法)
let slot = Router::key_to_slot(b"user:1000");  // 返回 0..16383

// 通过 slot 查找 group
let group_id = router.slot_to_group(slot)?;

// 直接路由 key 到 group
let group_id = router.route(&key)?;

// 获取 group 的所有副本节点
let nodes = router.route_to_nodes(&key)?;

// 获取 group leader
let leader = router.get_group_leader(group_id);

// 获取节点地址
let addr = router.get_node_address(node_id);

// 获取当前元数据版本
let version = router.get_version();

// 更新元数据缓存
router.update_metadata(new_meta);

// 从 MetaRaft 刷新元数据
router.refresh_metadata()?;
```

**注意**: `Router::key_to_slot()` 使用 CRC16/XMODEM 算法，与 Redis Cluster 完全兼容。

### 2. MultiRaftNode - 多 Raft Group 节点

MultiRaftNode 管理一个节点上的所有 Raft Group。

```rust
use aidb::cluster::MultiRaftNode;
use openraft::Config;

// 创建节点
let config = Config::default();
let node = MultiRaftNode::new(node_id, "./data", config).await?;

// 初始化 MetaRaft
node.init_meta_raft(config).await?;

// 初始化 MetaRaft 集群 (仅首节点)
node.initialize_meta_cluster(vec![(1, "127.0.0.1:50051".to_string())]).await?;

// 创建 Raft Group
let raft = node.create_raft_group(group_id, replicas).await?;

// 获取 Raft Group
let raft = node.get_raft_group(group_id);

// 移除 Raft Group
node.remove_raft_group(group_id).await?;

// 列出所有 Groups
let groups = node.list_groups();

// 数据操作 (带自动路由)
node.put(key, value).await?;
let value = node.get(&key)?;
node.delete(&key).await?;

// 启动节点
node.start(is_bootstrap, meta_leader_addr).await?;

// 关闭节点
node.shutdown().await?;
```

### 3. MetaRaftNode - 集群元数据管理

MetaRaftNode 通过 Raft 共识管理全局集群元数据。

```rust
use aidb::cluster::MetaRaftNode;

// 创建 MetaRaft 节点
let meta_raft = MetaRaftNode::new(node_id, "./data/meta", config).await?;

// 获取集群元数据
let meta: ClusterMeta = meta_raft.get_cluster_meta();

// 节点管理
meta_raft.add_node(node_id, addr).await?;
meta_raft.remove_node(node_id).await?;

// Group 管理
meta_raft.create_group(group_id, replicas).await?;
meta_raft.update_group_members(group_id, new_replicas).await?;
meta_raft.update_group_leader(group_id, leader).await?;

// Slot 管理
meta_raft.update_slots(start_slot, end_slot, group_id).await?;

// 迁移管理
meta_raft.start_migration(slot, from_group, to_group).await?;
meta_raft.complete_migration(slot).await?;

// Leader 查询
let is_leader = meta_raft.is_leader().await;
let leader_id = meta_raft.get_leader().await;
```

### 4. MigrationManager - Slot 迁移管理

MigrationManager 处理 slot 在线迁移，支持双写和原子切换。

```rust
use aidb::cluster::{MigrationManager, MigrationConfig};
use std::time::Duration;

// 创建迁移管理器
let config = MigrationConfig {
    batch_size: 100,
    rate_limit: 1000,  // keys/sec
    key_timeout: Duration::from_secs(5),
    max_retries: 3,
    batch_delay: Duration::from_millis(10),
};
let manager = MigrationManager::new(config, router, state_machine);

// 设置 MetaRaft (用于自动更新元数据)
let manager = manager.with_meta_raft(meta_raft);

// 启动迁移 worker
let handle = manager.start_worker();

// 开始 slot 迁移
manager.start_migration(slot, from_group, to_group).await?;

// 查询迁移进度
let progress = manager.get_migration_progress(slot);
let active = manager.get_active_migrations();
let is_migrating = manager.is_migrating(slot);

// 取消迁移
manager.cancel_migration(slot);

// 迁移感知的读写操作 (双写期间使用)
manager.put_with_migration_awareness(&key, value)?;
let value = manager.get_with_migration_awareness(&key)?;
manager.delete_with_migration_awareness(&key)?;

// 获取迁移指标
use std::sync::atomic::Ordering;
let metrics = manager.metrics();
println!("Keys migrated: {}", metrics.keys_migrated.load(Ordering::Relaxed));
println!("Success rate: {:.2}%", metrics.success_rate());
```

### 5. MembershipCoordinator - 成员变更协调

MembershipCoordinator 处理 Raft Group 成员变更。

```rust
use aidb::cluster::MembershipCoordinator;

// 创建协调器
let coordinator = MembershipCoordinator::new(node, meta_raft);

// 应用成员变更
coordinator.apply_membership_change(group_id, new_members).await?;

// 批量成员变更
coordinator.apply_membership_changes(vec![
    (group1, members1),
    (group2, members2),
]).await?;

// 添加 learner
coordinator.add_learner(group_id, node_id, addr).await?;

// 提升 learner 为 voter
coordinator.promote_learner(group_id, new_members).await?;

// 检查 group 是否准备好进行成员变更
let ready = coordinator.is_group_ready(group_id).await;
```

### 6. ReplicaAllocator - 副本分配

ReplicaAllocator 提供副本分配算法。

```rust
use aidb::cluster::ReplicaAllocator;

// 创建分配器 (3 副本)
let allocator = ReplicaAllocator::new(3);

// 为新 group 分配副本
let replicas = allocator.allocate_replicas(
    group_id,
    &available_nodes,
    &current_allocation,
)?;

// 重新平衡副本分配
let new_allocation = allocator.rebalance(&available_nodes, current_allocation)?;
```

### 7. ClusterMeta - 集群元数据结构

ClusterMeta 是全局集群状态的数据结构。

```rust
use aidb::cluster::{ClusterMeta, GroupMeta, NodeInfo, NodeStatus};

// 创建集群元数据
let meta = ClusterMeta::new();

// 创建均匀分布的 slot 映射
let meta = ClusterMeta::with_uniform_distribution(16);  // 16 个 groups

// 查询 slot 对应的 group
let group_id = meta.slot_to_group(slot);

// 获取 slot 对应的 group 元数据
let group = meta.get_group_for_slot(slot);

// 更新 slot 映射
meta.update_slot(slot, new_group_id);
meta.update_slot_range(start, end, group_id);

// Group 元数据
let group = GroupMeta::new(group_id, vec![1, 2, 3]);
group.set_leader(1);
let is_replica = group.is_replica(node_id);

// 节点信息
let node = NodeInfo::new(node_id, "127.0.0.1:50051".to_string());
node.set_online();
let is_online = node.is_online();
```

---

## 💡 使用示例

### 示例 1: 实现 CLUSTER KEYSLOT

```rust
use aidb::cluster::Router;

fn cluster_keyslot(key: &[u8]) -> u16 {
    Router::key_to_slot(key)
}

// 使用
let slot = cluster_keyslot(b"user:1000");
println!("Slot: {}", slot);  // 与 Redis CLUSTER KEYSLOT 结果一致
```

### 示例 2: 实现 CLUSTER INFO

```rust
use aidb::cluster::{MetaRaftNode, ClusterMeta, NodeStatus};

fn cluster_info(meta_raft: &MetaRaftNode) -> String {
    let meta = meta_raft.get_cluster_meta();
    
    // 统计已分配的 slots
    let assigned_slots = meta.slots.iter().filter(|&&g| g > 0).count();
    
    // 统计在线节点
    let known_nodes = meta.nodes.len();
    let online_nodes = meta.nodes.values()
        .filter(|n| matches!(n.status, NodeStatus::Online))
        .count();
    
    // 判断集群状态
    let cluster_state = if assigned_slots == 16384 && online_nodes > 0 {
        "ok"
    } else {
        "fail"
    };
    
    format!(
        "cluster_state:{}\n\
         cluster_slots_assigned:{}\n\
         cluster_slots_ok:{}\n\
         cluster_known_nodes:{}\n\
         cluster_size:{}",
        cluster_state,
        assigned_slots,
        assigned_slots,
        known_nodes,
        meta.groups.len()
    )
}
```

### 示例 3: 实现 CLUSTER NODES

```rust
use aidb::cluster::{MetaRaftNode, NodeStatus};

fn cluster_nodes(meta_raft: &MetaRaftNode) -> Vec<String> {
    let meta = meta_raft.get_cluster_meta();
    let mut result = Vec::new();
    
    for (node_id, info) in &meta.nodes {
        let status = match info.status {
            NodeStatus::Online => "connected",
            NodeStatus::Offline => "disconnected",
            _ => "handshake",
        };
        
        // 查找该节点负责的 slots
        let slots: Vec<String> = meta.groups.iter()
            .filter(|(_, g)| g.replicas.contains(node_id))
            .flat_map(|(_, g)| {
                if let Some((start, end)) = g.slot_range {
                    vec![format!("{}-{}", start, end)]
                } else {
                    vec![]
                }
            })
            .collect();
        
        result.push(format!(
            "{} {}:0 master - 0 0 {} {} {}",
            node_id,
            info.addr,
            meta.config_version,
            status,
            slots.join(" ")
        ));
    }
    
    result
}
```

### 示例 4: 实现 CLUSTER MEET

```rust
use aidb::cluster::MetaRaftNode;
use aidb::error::Error;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

async fn cluster_meet(meta_raft: &MetaRaftNode, addr: &str) -> Result<u64, Error> {
    // 生成新节点 ID (使用地址哈希)
    let node_id = {
        let mut hasher = DefaultHasher::new();
        addr.hash(&mut hasher);
        hasher.finish()
    };
    
    // 添加节点到集群元数据
    meta_raft.add_node(node_id, addr.to_string()).await?;
    
    Ok(node_id)
}
```

### 示例 5: 实现 slot 迁移

```rust
use aidb::cluster::{MigrationManager, Router};
use aidb::error::Error;
use std::time::Duration;

async fn migrate_slot(
    manager: &MigrationManager,
    router: &Router,
    slot: u16,
    target_group: u64,
) -> Result<(), Error> {
    // 获取当前 slot 所属的 group
    let from_group = router.slot_to_group(slot)?;
    
    // 开始迁移
    manager.start_migration(slot, from_group, target_group).await?;
    
    // 等待迁移完成
    loop {
        if let Some(progress) = manager.get_migration_progress(slot) {
            if progress.is_complete() {
                break;
            }
            println!("Migration progress: {:.2}%", progress.progress_pct());
        } else {
            break;  // 迁移已完成并清理
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    
    Ok(())
}
```

### 示例 6: 带路由的读写操作

```rust
use aidb::cluster::MultiRaftNode;
use aidb::error::Error;

async fn redis_set(node: &MultiRaftNode, key: &[u8], value: &[u8]) -> Result<(), Error> {
    // MultiRaftNode.put() 自动处理路由
    node.put(key.to_vec(), value.to_vec()).await
}

fn redis_get(node: &MultiRaftNode, key: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    // MultiRaftNode.get() 自动处理路由
    node.get(key)
}

async fn redis_del(node: &MultiRaftNode, key: &[u8]) -> Result<(), Error> {
    // MultiRaftNode.delete() 自动处理路由
    node.delete(key).await
}
```

### 示例 7: -MOVED 重定向

```rust
use aidb::cluster::{Router, MetaRaftNode};

fn handle_command_with_redirect(
    router: &Router,
    meta_raft: &MetaRaftNode,
    key: &[u8],
    local_node_id: u64,
) -> Result<RedirectAction, Error> {
    let slot = Router::key_to_slot(key);
    let group_id = router.slot_to_group(slot)?;
    
    // 检查 leader 是否在本节点
    if let Some(leader_id) = router.get_group_leader(group_id) {
        if leader_id == local_node_id {
            // 本地处理
            return Ok(RedirectAction::HandleLocally);
        }
        
        // 需要重定向
        if let Some(addr) = router.get_node_address(leader_id) {
            return Ok(RedirectAction::MovedTo(slot, addr));
        }
    }
    
    Err(Error::Internal("No leader found".to_string()))
}

enum RedirectAction {
    HandleLocally,
    MovedTo(u16, String),  // slot, addr
}
```

---

## 📝 注意事项

1. **CRC16 兼容性**: `Router::key_to_slot()` 使用 CRC16/XMODEM 算法，与 Redis Cluster 完全兼容。

2. **16384 Slots**: AiDb 使用与 Redis Cluster 相同的 16384 个 slots。

3. **自动路由**: `MultiRaftNode` 的 `put/get/delete` 方法已内置自动路由逻辑。

4. **迁移感知**: 在 slot 迁移期间，建议使用 `MigrationManager` 的迁移感知方法确保数据一致性。

5. **元数据缓存**: `Router` 维护本地元数据缓存，可通过 `refresh_metadata()` 手动刷新或使用 `start_watching()` 自动同步。

6. **Feature 依赖**: 所有集群 API 需要启用 `cluster` feature (`aidb/raft-cluster`)。

7. **🆕 同步 Raft 共识**: `CLUSTER MEET` 和 `CLUSTER FORGET` 命令会 **同步等待** Raft 共识完成（超时 5 秒），确保命令返回 OK 时集群元数据已同步到所有节点。这解决了元数据收敛延迟问题。

---

## 📚 相关文档

- [TODO.md](../TODO.md) - 详细实现计划
- [AiDb GitHub](https://github.com/Genuineh/AiDb) - AiDb 源码仓库

---

*文档版本: v1.0*  
*最后更新: 2025-11-25*
