# AiKv 部署指南

## 概述

本文档提供了 AiKv Redis 协议兼容层的详细部署步骤和配置说明。

## 🚀 快速部署（推荐）

**最简单的部署方式是使用 aikv-tool:**

```bash
# 1. 安装 aikv-tool
cd aikv-toolchain && cargo install --path . && cd ..

# 2. 一键部署集群 (6 节点: 3 主 3 从)
aikv-tool cluster setup

# 3. 验证集群
aikv-tool cluster status

# 4. 开始使用
redis-cli -c -h 127.0.0.1 -p 6379
```

**一条命令完成所有部署工作！** 详见下方 [使用 aikv-tool 部署](#使用-aikv-tool-部署) 章节。

---

## 系统要求

### 最低要求

- **操作系统**: Linux (Ubuntu 20.04+, CentOS 8+), macOS 10.15+, Windows 10+
- **架构**: x86_64 或 ARM64
- **内存**: 至少 512MB 可用内存
- **磁盘空间**: 至少 1GB 可用空间
- **Rust**: 1.70.0 或更高版本

### 推荐配置

- **CPU**: 4 核心或更多
- **内存**: 4GB 或更多
- **磁盘**: SSD，10GB 或更多
- **网络**: 千兆网卡

## 安装步骤

### 1. 安装 Rust 工具链

如果还没有安装 Rust，请先安装：

```bash
# Linux/macOS
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 配置当前 shell
source $HOME/.cargo/env

# 验证安装
rustc --version
cargo --version
```

### 2. 克隆项目

```bash
git clone https://github.com/Genuineh/AiKv.git
cd AiKv
```

### 3. 编译项目

```bash
# 开发构建（包含调试信息）
cargo build

# 生产构建（优化版本）
cargo build --release
```

编译完成后，可执行文件位于：
- 开发版本: `target/debug/aikv`
- 生产版本: `target/release/aikv`

### 4. 运行测试

```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test string_commands
cargo test json_commands

# 运行测试并显示输出
cargo test -- --nocapture
```

## 配置

### 配置文件

创建配置文件 `config.toml`:

```toml
# 已实现的配置项 / Implemented options
[server]
# 服务器监听地址
host = "127.0.0.1"
# 服务器监听端口
port = 6379
# 最大连接数（默认 10000）
max_connections = 10000

[storage]
# 存储引擎类型: "memory" 或 "aidb"
engine = "memory"
# 数据存储目录（aidb 模式需要）
data_dir = "./data"
# 数据库数量（默认 16）
databases = 16

[logging]
# 日志级别: trace, debug, info, warn, error
level = "info"
```

> **注意**: 完整的配置模板请参考 `config/aikv.toml`。配置文件中标记为 🚧 的选项尚未实现。

### 环境变量

> **注意**: 环境变量覆盖功能尚未实现。请使用配置文件或命令行参数。

## 启动服务

### 开发模式

```bash
# 使用默认配置启动
cargo run

# 使用指定配置文件启动
cargo run -- --config config.toml

# 指定端口启动
cargo run -- --port 6380
```

### 生产模式

```bash
# 先编译 release 版本
cargo build --release

# 启动服务
./target/release/aikv --config config.toml

# 后台启动（使用 nohup）
nohup ./target/release/aikv --config config.toml > aikv.log 2>&1 &

# 后台启动（使用 systemd，见下文）
systemctl start aikv
```

### 命令行参数

```
aikv [OPTIONS]

OPTIONS:
    -c, --config <FILE>       配置文件路径 (TOML 格式)
    -H, --host <HOST>         监听地址 [默认: 127.0.0.1]
    -p, --port <PORT>         监听端口 [默认: 6379]
    -h, --help                显示帮助信息
    -v, --version             显示版本信息

EXAMPLES:
    aikv                           # 使用默认配置启动
    aikv --config config.toml      # 使用配置文件
    aikv -H 0.0.0.0 -p 6380        # 指定主机和端口
    aikv 127.0.0.1:6379            # 旧版兼容模式
```

## 使用 Systemd 管理（Linux）

### 1. 创建 systemd 服务文件

创建文件 `/etc/systemd/system/aikv.service`:

```ini
[Unit]
Description=AiKv Redis Protocol Server
After=network.target

[Service]
Type=simple
User=aikv
Group=aikv
WorkingDirectory=/opt/aikv
ExecStart=/opt/aikv/aikv --config /opt/aikv/config.toml
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

# 安全设置
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/aikv/data /opt/aikv/logs

# 资源限制
LimitNOFILE=65535
LimitNPROC=4096

[Install]
WantedBy=multi-user.target
```

### 2. 创建专用用户

```bash
# 创建 aikv 用户
sudo useradd -r -s /bin/false aikv

# 创建目录
sudo mkdir -p /opt/aikv/{data,logs}

# 复制文件
sudo cp target/release/aikv /opt/aikv/
sudo cp config.toml /opt/aikv/

# 设置权限
sudo chown -R aikv:aikv /opt/aikv
sudo chmod 755 /opt/aikv/aikv
```

### 3. 启动和管理服务

```bash
# 重新加载 systemd 配置
sudo systemctl daemon-reload

# 启动服务
sudo systemctl start aikv

# 设置开机自启
sudo systemctl enable aikv

# 查看状态
sudo systemctl status aikv

# 停止服务
sudo systemctl stop aikv

# 重启服务
sudo systemctl restart aikv

# 查看日志
sudo journalctl -u aikv -f
```

## 使用 Docker 部署

### 1. 创建 Dockerfile

```dockerfile
# ============================================================
# AiKv Dockerfile - Multi-stage build for minimal image size
# ============================================================

# ------------------------------------------------------------
# Stage 1: Builder - Compile the Rust application
# ------------------------------------------------------------
FROM rust:1.75-bookworm AS builder
# Use Rust 1.75 (matches project requirement: Rust 1.70+)

# Build argument for enabling features (e.g., "cluster" for cluster support)
ARG FEATURES=""

# Install build dependencies
RUN apt-get update && apt-get install -y \
    cmake \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to build dependencies
RUN mkdir -p src benches && \
    echo 'fn main() { println!("Dummy"); }' > src/main.rs && \
    echo 'pub fn dummy() {}' > src/lib.rs

# Build dependencies only (this layer will be cached)
RUN cargo build --release --features $FEATURES && rm -rf src benches

# Copy actual source code
COPY src ./src

# Recreate dummy benchmark files
RUN mkdir -p benches && \
    echo 'fn main() {}' > benches/aikv_benchmark.rs && \
    echo 'fn main() {}' > benches/comprehensive_benchmark.rs

# Build the actual application
RUN cargo build --release --features $FEATURES --bin aikv

# Strip the binary to reduce size
RUN strip target/release/aikv

# ------------------------------------------------------------
# Stage 2: Runtime - Create minimal runtime image
# ------------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    redis-tools \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN groupadd --gid 1000 aikv && \
    useradd --uid 1000 --gid aikv --shell /bin/bash --create-home aikv

# Create directories
RUN mkdir -p /app/data /app/logs /app/config && \
    chown -R aikv:aikv /app

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /app/target/release/aikv /app/aikv

# Copy default configuration
COPY config/aikv.toml /app/config/aikv.toml

# Set ownership
RUN chown -R aikv:aikv /app

# Switch to non-root user
USER aikv

# Expose default port
EXPOSE 6379

# Health check using redis-cli
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD redis-cli PING | grep -q "PONG" || exit 1

# Default command
ENTRYPOINT ["/app/aikv"]
CMD ["--host", "0.0.0.0", "--port", "6379"]
```

### 2. 构建镜像

```bash
# 构建单机版
docker build -t aikv:latest .

# 构建集群版
docker build -t aikv:cluster --build-arg FEATURES=cluster .
```

### 3. 运行容器

```bash
# 简单运行
docker run -d \
  --name aikv \
  -p 6379:6379 \
  aikv:latest

# 挂载数据卷
docker run -d \
  --name aikv \
  -p 6379:6379 \
  -v $(pwd)/data:/app/data \
  -v $(pwd)/logs:/app/logs \
  -v $(pwd)/config.toml:/app/config.toml \
  aikv:latest

# 设置资源限制
docker run -d \
  --name aikv \
  -p 6379:6379 \
  --memory="1g" \
  --cpus="2" \
  aikv:latest
```

### 4. Docker Compose

创建 `docker-compose.yml`:

```yaml
version: '3.8'

services:
  aikv:
    build: .
    container_name: aikv
    ports:
      - "6379:6379"
    volumes:
      - ./data:/app/data
      - ./logs:/app/logs
      - ./config.toml:/app/config.toml
    restart: unless-stopped
    deploy:
      resources:
        limits:
          cpus: '2'
          memory: 1G
        reservations:
          cpus: '1'
          memory: 512M
```

运行：

```bash
# 启动
docker-compose up -d

# 停止
docker-compose down

# 查看日志
docker-compose logs -f
```

### 5. 使用 aikv-tool 部署（推荐）

aikv-tool 是 AiKv 官方的一站式部署工具，大大简化了部署流程。

#### 安装 aikv-tool

```bash
cd aikv-toolchain
cargo install --path .
cd ..
```

#### 一键部署集群

```bash
# 这一条命令完成所有工作！
aikv-tool cluster setup
```

该命令会自动：
1. ✅ 生成 Docker Compose 和节点配置文件
2. ✅ 构建带集群功能的 Docker 镜像
3. ✅ 启动 6 个节点容器
4. ✅ 初始化 MetaRaft 成员
5. ✅ 分配 16384 槽位
6. ✅ 配置主从复制

#### 集群管理命令

```bash
aikv-tool cluster setup      # 一键部署
aikv-tool cluster init       # 初始化集群配置
aikv-tool cluster start      # 启动集群
aikv-tool cluster stop       # 停止集群
aikv-tool cluster stop -v    # 停止并清理数据
aikv-tool cluster restart    # 重启集群
aikv-tool cluster status     # 查看状态
aikv-tool cluster logs       # 查看日志
aikv-tool cluster logs -f    # 实时日志

# 快捷方式
aikv-tool up                 # = cluster setup
aikv-tool down               # = cluster stop
```

#### 部署单节点

```bash
aikv-tool deploy -t single -o ./my-deploy
cd my-deploy
./start.sh
```

#### 其他功能

```bash
aikv-tool build --release    # 编译 release 版本
aikv-tool docker             # 构建 Docker 镜像
aikv-tool status             # 查看项目状态
aikv-tool tui                # 交互式界面
```

### 6. Docker Compose 集群部署（手动方式）

如果你更喜欢手动控制部署过程：

```bash
# 使用 aikv-tool 生成部署文件
aikv-tool deploy -t cluster -o ./deploy

# 构建镜像
aikv-tool docker --cluster --tag cluster

# 启动容器
cd deploy
docker-compose up -d

# 等待节点就绪
sleep 10

# 初始化集群
./init-cluster.sh

# 验证
redis-cli -c -p 6379 CLUSTER INFO
```

#### 手动停止和清理

```bash
cd deploy
docker-compose down      # 停止
docker-compose down -v   # 停止并清理数据
```

## 监控和维护

### 健康检查

```bash
# 使用 redis-cli 检查
redis-cli -h 127.0.0.1 -p 6379 ping

# 使用 telnet 检查
echo "PING" | nc 127.0.0.1 6379
```

### 日志管理

日志文件位置：`./logs/aikv.log`

```bash
# 查看实时日志
tail -f logs/aikv.log

# 搜索错误日志
grep "ERROR" logs/aikv.log

# 日志归档（建议定期执行）
tar -czf logs/aikv-$(date +%Y%m%d).tar.gz logs/aikv.log
```

### 数据备份

```bash
# 停止服务
systemctl stop aikv

# 备份数据
tar -czf backup-$(date +%Y%m%d).tar.gz data/

# 启动服务
systemctl start aikv

# 或使用热备份（如果支持）
cp -r data/ backup/data-$(date +%Y%m%d)/
```

### 性能监控

```bash
# 使用 redis-benchmark 测试性能
redis-benchmark -h 127.0.0.1 -p 6379 -q -t set,get

# 监控系统资源
top -p $(pgrep aikv)
htop

# 网络连接监控
netstat -an | grep 6379
ss -tn | grep 6379
```

## 故障排查

### 服务无法启动

1. 检查端口是否被占用：
```bash
lsof -i :6379
netstat -tlnp | grep 6379
```

2. 检查配置文件：
```bash
./aikv --config config.toml --validate
```

3. 查看日志：
```bash
tail -100 logs/aikv.log
journalctl -u aikv -n 100
```

### 连接被拒绝

1. 检查防火墙：
```bash
# Ubuntu/Debian
sudo ufw status
sudo ufw allow 6379

# CentOS/RHEL
sudo firewall-cmd --list-all
sudo firewall-cmd --add-port=6379/tcp --permanent
sudo firewall-cmd --reload
```

2. 检查监听地址：
```bash
netstat -tlnp | grep aikv
ss -tlnp | grep aikv
```

### 性能问题

1. 检查系统资源：
```bash
# CPU 使用率
top

# 内存使用
free -h

# 磁盘 I/O
iostat -x 1
```

2. 调整配置：
- 增加 `worker_threads`
- 调整 `max_connections`
- 启用 `tcp_nodelay`

### 内存泄漏

```bash
# 监控内存使用
watch -n 1 'ps aux | grep aikv'

# 使用 valgrind 检测（需要调试版本）
valgrind --leak-check=full ./target/debug/aikv
```

## 升级

### 平滑升级

```bash
# 1. 备份数据
tar -czf backup-$(date +%Y%m%d).tar.gz data/

# 2. 拉取新版本
git pull origin main

# 3. 编译新版本
cargo build --release

# 4. 停止服务
systemctl stop aikv

# 5. 替换可执行文件
sudo cp target/release/aikv /opt/aikv/

# 6. 启动服务
systemctl start aikv

# 7. 验证
redis-cli -h 127.0.0.1 -p 6379 ping
```

## 安全建议

1. **网络隔离**: 不要将服务直接暴露在公网
2. **访问控制**: 使用防火墙限制访问源 IP
3. **数据加密**: 考虑使用 TLS/SSL（未来版本支持）
4. **定期备份**: 设置自动备份任务
5. **监控告警**: 设置监控和告警机制
6. **及时更新**: 关注安全更新并及时升级

## 性能调优

### 操作系统层面

```bash
# 增加文件描述符限制
echo "* soft nofile 65535" >> /etc/security/limits.conf
echo "* hard nofile 65535" >> /etc/security/limits.conf

# TCP 优化
sysctl -w net.ipv4.tcp_tw_reuse=1
sysctl -w net.core.somaxconn=65535
sysctl -w net.ipv4.tcp_max_syn_backlog=65535
```

### 应用层面

> **注意**: 以下配置项尚未实现，将在后续版本中添加。
> **Note**: The following options are not yet implemented.

在 `config.toml` 中调整（计划中的功能）：

```toml
# 🚧 以下配置尚未实现 / Not yet implemented
# [performance]
# worker_threads = 4  # 根据 CPU 核心数调整
# tcp_nodelay = true
# tcp_keepalive = true
#
# [storage]
# max_memory = "2GB"  # 根据可用内存调整
```

当前可用的配置：

```toml
[storage]
# 选择合适的存储引擎
engine = "aidb"  # 使用 AiDb 持久化存储
data_dir = "./data"
databases = 16

[logging]
level = "info"  # 生产环境推荐使用 info 或 warn
```

## 支持与帮助

- GitHub Issues: https://github.com/Genuineh/AiKv/issues
- 文档: https://github.com/Genuineh/AiKv/tree/main/docs

## 许可证

本项目采用 MIT 许可证。详见 [LICENSE](../LICENSE) 文件。
