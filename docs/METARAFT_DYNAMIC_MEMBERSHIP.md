# Dynamic MetaRaft Membership - Usage Guide

本文档说明如何使用动态 MetaRaft 成员变更功能构建多主节点集群。

## 概述

从 AiKv v0.2.0 开始，支持动态添加和提升 MetaRaft 节点，无需预配置 peers 列表。

### 工作流程

1. **Bootstrap 节点**以单节点模式初始化 MetaRaft
2. **其他节点**启动后作为 learner 加入 MetaRaft
3. 通过 **CLUSTER METARAFT PROMOTE** 提升 learner 为 voter
4. 所有主节点成为 MetaRaft voters，可以提议 Raft 变更

## API 参考

### ClusterNode API

```rust
// 添加节点为 MetaRaft learner
cluster_node.add_meta_learner(node_id: u64, addr: String).await?;

// 提升 learner 为 voter
cluster_node.promote_meta_voter(voters: Vec<u64>).await?;

// 直接变更成员（低级 API）
cluster_node.change_meta_membership(voters: Vec<u64>, retain_learners: bool).await?;
```

### Redis 命令

```redis
# 添加节点 2 为 learner
CLUSTER METARAFT ADDLEARNER 2 127.0.0.1:50052

# 提升节点 2 为 voter（需包含所有期望的 voters）
CLUSTER METARAFT PROMOTE 1 2

# 查看成员列表
CLUSTER METARAFT MEMBERS
```

## 使用示例

### 示例 1: 构建 3 节点 MetaRaft 集群

#### 步骤 1: 启动 Bootstrap 节点

```bash
# Node 1 (Bootstrap)
aikv --node-id 1 \
     --bind 127.0.0.1:6379 \
     --raft-addr 127.0.0.1:50051 \
     --cluster-enabled \
     --is-bootstrap
```

此时 Node 1 是单节点 MetaRaft 集群的唯一 voter。

#### 步骤 2: 启动其他节点

```bash
# Node 2
aikv --node-id 2 \
     --bind 127.0.0.1:6380 \
     --raft-addr 127.0.0.1:50052 \
     --cluster-enabled

# Node 3
aikv --node-id 3 \
     --bind 127.0.0.1:6381 \
     --raft-addr 127.0.0.1:50053 \
     --cluster-enabled
```

#### 步骤 3: 将节点 2 添加为 learner

```bash
redis-cli -p 6379 CLUSTER METARAFT ADDLEARNER 2 127.0.0.1:50052
```

#### 步骤 4: 提升节点 2 为 voter

```bash
redis-cli -p 6379 CLUSTER METARAFT PROMOTE 1 2
```

现在 Node 1 和 Node 2 都是 voters。

#### 步骤 5: 重复添加节点 3

```bash
# 添加为 learner
redis-cli -p 6379 CLUSTER METARAFT ADDLEARNER 3 127.0.0.1:50053

# 提升为 voter
redis-cli -p 6379 CLUSTER METARAFT PROMOTE 1 2 3
```

#### 步骤 6: 验证集群状态

```bash
redis-cli -p 6379 CLUSTER METARAFT MEMBERS
```

预期输出：
```
1) 1) "1"
   2) "voter"
2) 1) "2"
   2) "voter"
3) 1) "3"
   2) "voter"
```

### 示例 2: 使用 Rust API

```rust
use aikv::cluster::{ClusterConfig, ClusterNode};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建 Bootstrap 节点
    let config1 = ClusterConfig {
        node_id: 1,
        data_dir: PathBuf::from("./data/node1"),
        bind_address: "127.0.0.1:6379".to_string(),
        raft_address: "127.0.0.1:50051".to_string(),
        num_groups: 4,
        is_bootstrap: true,
        initial_members: vec![(1, "127.0.0.1:50051".to_string())],
    };

    let mut node1 = ClusterNode::new(config1);
    node1.initialize().await?;

    // 添加节点 2 为 learner
    node1.add_meta_learner(2, "127.0.0.1:50052".to_string()).await?;

    // 等待 learner 同步日志
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // 提升节点 2 为 voter
    node1.promote_meta_voter(vec![1, 2]).await?;

    println!("Node 2 promoted to voter!");

    Ok(())
}
```

## 最佳实践

### 1. 单节点 Bootstrap

始终使用单节点模式初始化 MetaRaft：

```rust
initial_members: vec![(1, "127.0.0.1:50051".to_string())]
```

### 2. 等待 Learner 同步

在提升 learner 为 voter 之前，等待其同步日志：

```bash
# 等待 1-2 秒后再提升
sleep 2
redis-cli -p 6379 CLUSTER METARAFT PROMOTE 1 2
```

### 3. 包含所有 Voters

提升时必须包含所有期望的 voters：

```bash
# 错误：只指定新 voter
CLUSTER METARAFT PROMOTE 2

# 正确：包含所有 voters
CLUSTER METARAFT PROMOTE 1 2
```

### 4. 验证成员状态

提升后验证成员角色：

```bash
redis-cli -p 6379 CLUSTER METARAFT MEMBERS
```

## 故障排查

### 问题 1: Learner 添加失败

**症状**: `CLUSTER METARAFT ADDLEARNER` 返回错误

**解决方案**:
1. 确认目标节点已启动
2. 确认网络连接正常
3. 检查地址格式正确（`ip:port`）

### 问题 2: Promote 失败

**症状**: `CLUSTER METARAFT PROMOTE` 返回错误

**解决方案**:
1. 确认 learner 已成功添加
2. 等待 learner 同步日志
3. 包含所有期望的 voters（包括现有 voters）

### 问题 3: 成员列表不更新

**症状**: `CLUSTER METARAFT MEMBERS` 显示旧数据

**解决方案**:
1. 等待 Raft 共识完成（通常 < 1 秒）
2. 检查是否在 leader 节点执行命令
3. 验证网络连接

## 技术细节

### OpenRaft 集成

AiKv 使用 AiDb v0.5.2 的 Multi-Raft 实现，底层基于 OpenRaft：

- `add_learner()`: 添加非投票成员
- `change_membership()`: 通过 Joint Consensus 变更成员

### 零停机变更

OpenRaft 的 Joint Consensus 确保成员变更期间集群保持可用：

1. 进入 Joint 配置（C_old + C_new）
2. 等待 Joint 配置提交
3. 转换到新配置（C_new）

### 日志复制

Learner 接收所有日志条目但不参与：
- 领导者选举
- 日志提交决策

提升为 voter 后，节点立即参与投票。

## 参考文档

- [TODO.md](../TODO.md) - 详细实现计划
- [AIDB_CLUSTER_API_REFERENCE.md](AIDB_CLUSTER_API_REFERENCE.md) - 完整 API 参考
- [OpenRaft Documentation](https://docs.rs/openraft/) - OpenRaft 官方文档

---

*文档版本: v1.0*  
*创建时间: 2025-12-15*
