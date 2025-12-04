# ============================================================
# AiKv Dockerfile - Multi-stage build for minimal image size
# ============================================================
#
# Build (standalone):
#   docker build -t aikv:latest .
#
# Build (with cluster support):
#   docker build -t aikv:cluster --build-arg FEATURES=cluster .
#
# Run:
#   docker run -d -p 6379:6379 aikv:latest
#
# For development with hot reload:
#   docker-compose -f docker-compose.dev.yml up
#
# For cluster deployment:
#   docker-compose -f docker-compose.cluster.yml up
#
# ============================================================

# ------------------------------------------------------------
# Stage 1: Builder - Compile the Rust application
# ------------------------------------------------------------
FROM rust:1.82-bookworm AS builder

# Build argument for enabling features (e.g., "cluster" for cluster support)
ARG FEATURES=""

# Set features flag for cargo commands
ENV CARGO_FEATURES="${FEATURES:+--features $FEATURES}"

# Install build dependencies
RUN apt-get update && apt-get install -y \
    cmake \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Create a new directory for our application
WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy source and benchmark files to build dependencies
RUN mkdir -p src benches && \
    echo 'fn main() { println!("Dummy"); }' > src/main.rs && \
    echo 'pub fn dummy() {}' > src/lib.rs && \
    echo 'fn main() {}' > benches/aikv_benchmark.rs && \
    echo 'fn main() {}' > benches/comprehensive_benchmark.rs

# Build dependencies only (this layer will be cached)
# Use features if specified (e.g., cluster)
RUN cargo build --release $CARGO_FEATURES \
    && rm -rf src benches

# Copy actual source code (only what's needed for building the binary)
COPY src ./src

# Touch main.rs to ensure rebuild
RUN touch src/main.rs src/lib.rs

# Build the actual application with specified features
RUN cargo build --release $CARGO_FEATURES --bin aikv

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

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD echo "PING" | nc -w 1 localhost 6379 | grep -q "PONG" || exit 1

# Default command
ENTRYPOINT ["/app/aikv"]
CMD ["--host", "0.0.0.0", "--port", "6379"]

# Labels
LABEL org.opencontainers.image.title="AiKv" \
      org.opencontainers.image.description="Redis protocol compatible key-value store based on AiDb" \
      org.opencontainers.image.version="0.1.0" \
      org.opencontainers.image.vendor="Genuineh" \
      org.opencontainers.image.source="https://github.com/Genuineh/AiKv" \
      org.opencontainers.image.licenses="MIT"
