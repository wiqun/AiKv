# AiKv 文档中心

欢迎来到 AiKv 文档中心！本文档帮助你快速找到所需信息。

## 🚀 30 秒快速开始

```bash
# 1. 安装 aikv-tool (项目根目录)
cd aikv-toolchain && cargo install --path . && cd ..

# 2. 一键部署集群 (推荐方式)
aikv-tool cluster setup

# 3. 连接使用
redis-cli -c -h 127.0.0.1 -p 6379
```

就这么简单！更多选项请查看下方详细文档。

---

## 📚 文档导航

### 🎯 新手必读

| 文档 | 说明 |
|------|------|
| [快速开始指南](QUICK_START.md) | 5 分钟入门 AiKv |
| [API 参考](API.md) | 100+ 命令完整参考 |
| [部署指南](DEPLOYMENT.md) | 单节点和集群部署 |

### 🏗️ 架构设计

| 文档 | 说明 |
|------|------|
| [系统架构](ARCHITECTURE.md) | 整体架构设计 |
| [架构重构](ARCHITECTURE_REFACTORING.md) | 存储层架构演进 |
| [开发计划](DEVELOPMENT_PLAN.md) | 8 阶段开发规划 |

### 🌐 集群专题

| 文档 | 说明 |
|------|------|
| [Cluster API](AIDB_CLUSTER_API_REFERENCE.md) | 集群命令参考 |
| [MetaRaft 动态成员](METARAFT_DYNAMIC_MEMBERSHIP.md) | 动态成员管理 |
| [Cluster Bus 分析](CLUSTER_BUS_ANALYSIS.md) | 节点通信分析 |
| [集群故障排除](CLUSTER_TROUBLESHOOTING.md) | 常见问题解决 |
| [元数据同步修复](CLUSTER_METADATA_SYNC_FIX.md) | 同步问题修复 |

### 💾 存储与持久化

| 文档 | 说明 |
|------|------|
| [AiDb 集成](AIDB_INTEGRATION.md) | LSM-Tree 存储引擎 |
| [持久化](PERSISTENCE.md) | 数据持久化机制 |

### 📜 特性专题

| 文档 | 说明 |
|------|------|
| [Lua 脚本](LUA_SCRIPTING.md) | Lua 脚本完整指南 |
| [Lua 事务设计](LUA_TRANSACTION_DESIGN.md) | 事务性回滚设计 |

### ⚡ 性能与运维

| 文档 | 说明 |
|------|------|
| [性能基准](PERFORMANCE.md) | 性能测试报告 |
| [性能调优](PERFORMANCE_TUNING.md) | 性能优化指南 |
| [最佳实践](BEST_PRACTICES.md) | 生产环境建议 |
| [故障排除](TROUBLESHOOTING.md) | 通用问题解决 |

### 📦 归档文档

历史升级记录和已完成任务的文档已移至 [archive/](archive/) 目录。

---

## 🔧 aikv-tool 工具

AiKv 提供了一站式管理工具 `aikv-tool`，支持：

### 集群管理（推荐使用）

```bash
# 一键部署集群（生成配置 → 构建镜像 → 启动 → 初始化）
aikv-tool cluster setup

# 分步操作
aikv-tool cluster start    # 启动集群
aikv-tool cluster init     # 初始化集群
aikv-tool cluster status   # 查看状态
aikv-tool cluster stop     # 停止集群
```

### 其他功能

```bash
aikv-tool build            # 编译项目
aikv-tool docker           # 构建 Docker 镜像
aikv-tool deploy           # 生成部署文件
aikv-tool bench            # 运行性能测试
aikv-tool status           # 查看项目状态
aikv-tool tui              # 交互式 TUI 界面
```

详细用法请查看 [aikv-toolchain/README.md](../aikv-toolchain/README.md)。

---

## 📊 项目状态

```
总体进度: ████████████████████░░░ 90%

✅ 已完成:
  • RESP2/RESP3 协议支持
  • 100+ Redis 命令
  • 双存储引擎 (Memory/AiDb)
  • Lua 脚本 (事务性)
  • 集群命令 (17 个)
  • MetaRaft 动态成员
  • 槽迁移支持
  • 主从复制

⏳ 进行中:
  • Cluster Bus (gossip)
  • 自动故障转移
  • 官方测试套件
```

---

## 🔗 快速链接

- [项目主页](../README.md)
- [TODO 列表](../TODO.md)
- [变更日志](../CHANGELOG.md)
- [贡献指南](../CONTRIBUTING.md)

---

*最后更新: 2026-01-06*
