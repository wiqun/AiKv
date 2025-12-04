# AiKv Toolchain

🔧 AiKv 项目管理工具链 - 使用 Rust + Ratatui 构建的 TUI 工具

## 功能特性

- 🔨 **构建 AiKv** - 支持单节点和集群模式编译
- 🐳 **构建 Docker 镜像** - 一键构建容器镜像
- 📦 **生成部署文件** - 自动生成 docker-compose 和配置文件
- ⚙️ **配置文档** - 详细的配置说明和选项
- 📊 **性能测试** - 运行基准测试
- 🚀 **优化建议** - 系统和应用层优化指南
- 📖 **项目文档** - 完整的使用和部署文档
- ℹ️ **项目状态** - 查看项目和系统信息

## 安装

```bash
# 在 AiKv 项目根目录下
cd aikv-toolchain

# 编译
cargo build --release

# 安装到 cargo bin 目录
cargo install --path .
```

## 使用方法

### TUI 界面（默认）

```bash
# 启动 TUI 界面
aikv-tool

# 或者
aikv-tool tui
```

### 命令行模式

```bash
# 构建 AiKv
aikv-tool build                    # 开发构建
aikv-tool build --release          # 发布构建
aikv-tool build --release --cluster # 集群版本

# 构建 Docker 镜像
aikv-tool docker                   # 标准镜像
aikv-tool docker --cluster         # 集群镜像
aikv-tool docker --tag v0.1.0      # 指定标签

# 生成部署文件
aikv-tool deploy                   # 单节点部署
aikv-tool deploy -t cluster        # 集群部署
aikv-tool deploy -o ./my-deploy    # 指定输出目录

# 查看配置文档
aikv-tool config                   # 单节点配置
aikv-tool config --cluster         # 集群配置

# 运行基准测试
aikv-tool bench                    # 快速测试
aikv-tool bench -t full            # 完整测试

# 查看优化建议
aikv-tool optimize

# 查看文档
aikv-tool docs                     # 通用文档
aikv-tool docs --topic api         # API 文档
aikv-tool docs --topic deploy      # 部署文档
aikv-tool docs --topic performance # 性能文档
aikv-tool docs --topic cluster     # 集群文档

# 查看项目状态
aikv-tool status
```

## TUI 键盘快捷键

### 主菜单
- `↑/k` - 向上移动
- `↓/j` - 向下移动
- `Enter` - 选择
- `q` - 退出

### 构建选项
- `r` - 切换 Release 模式
- `c` - 切换 Cluster 特性
- `b/Enter` - 开始构建
- `q/Esc` - 返回菜单

### 部署选项
- `t` - 切换部署类型 (单节点/集群)
- `+/-` - 调整节点数量
- `g/Enter` - 生成部署文件
- `q/Esc` - 返回菜单

### 文档/配置视图
- `↑/k` - 向上滚动
- `↓/j` - 向下滚动
- `PageUp/PageDown` - 快速滚动
- `c` - 切换配置模式 (单节点/集群)
- `q/Esc` - 返回菜单

## 生成的部署文件

### 单节点部署
```
deploy/
├── docker-compose.yml   # Docker Compose 配置
├── aikv.toml            # AiKv 配置文件
├── README.md            # 部署说明
├── start.sh             # 启动脚本
└── stop.sh              # 停止脚本
```

### 集群部署
```
deploy/
├── docker-compose.yml   # 6 节点集群配置
├── aikv-node1.toml      # 节点 1 配置
├── aikv-node2.toml      # 节点 2 配置
├── aikv-node3.toml      # 节点 3 配置
├── aikv-node4.toml      # 节点 4 配置
├── aikv-node5.toml      # 节点 5 配置
├── aikv-node6.toml      # 节点 6 配置
├── README.md            # 集群部署说明
├── start.sh             # 启动脚本
├── stop.sh              # 停止脚本
└── init-cluster.sh      # 集群初始化脚本
```

## 快速开始

### 1. 构建 AiKv

```bash
aikv-tool build --release
```

### 2. 生成单节点部署

```bash
aikv-tool deploy -t single -o ./deploy-single
cd deploy-single
./start.sh
```

### 3. 生成集群部署

```bash
aikv-tool deploy -t cluster -o ./deploy-cluster
cd deploy-cluster
./start.sh
./init-cluster.sh
```

## 技术栈

- **Ratatui** - 终端用户界面框架
- **Crossterm** - 跨平台终端操作
- **Tokio** - 异步运行时
- **Clap** - 命令行参数解析
- **Serde** - 序列化/反序列化

## 许可证

MIT License
