# AiKv Performance Benchmarking Guide

This guide explains how to run performance benchmarks for AiKv and interpret the results.

## Quick Start

### 1. Run Built-in Criterion Benchmarks

Run all benchmarks:
```bash
cargo bench
```

Run specific benchmark suite:
```bash
# Original benchmark suite
cargo bench --bench aikv_benchmark

# Comprehensive benchmark suite
cargo bench --bench comprehensive_benchmark
```

### 2. Run Redis-Benchmark Tests

First, start the AiKv server:
```bash
cargo run --release
```

In another terminal, run the benchmark script:
```bash
./scripts/benchmark.sh
```

## Benchmark Suites

### aikv_benchmark.rs
Basic performance benchmarks covering:
- **RESP Encoding**: Simple string, integer, bulk string, array
- **RESP Parsing**: Simple string, integer, bulk string, array
- **Storage Operations**: SET, GET, EXISTS, DELETE
- **Multi-Key Operations**: MSET/MGET with 10, 100, 1000 keys

### comprehensive_benchmark.rs
Advanced performance benchmarks covering:
- **Concurrent Operations**: 2, 4, 8 thread scenarios
- **Large Values**: 1KB, 10KB, 100KB, 1MB data handling
- **RESP Parsing Sizes**: Arrays with 1, 10, 100, 1000 elements
- **Batch Sizes**: Operations with 10, 50, 100, 500, 1000 items
- **Memory Patterns**: Allocation/deallocation, growing datasets
- **RESP3 Types**: Null, boolean, double, map encoding

## Redis-Benchmark Script

### Basic Usage
```bash
./scripts/benchmark.sh
```

### Configuration
Use environment variables to customize:

```bash
# Host and port
AIKV_HOST=127.0.0.1 AIKV_PORT=6379 ./scripts/benchmark.sh

# Number of requests
AIKV_REQUESTS=100000 ./scripts/benchmark.sh

# Number of parallel clients
AIKV_CLIENTS=50 ./scripts/benchmark.sh

# Pipeline depth
AIKV_PIPELINE=16 ./scripts/benchmark.sh

# Data sizes
AIKV_VALUE_SIZE=256 ./scripts/benchmark.sh

# Output directory
AIKV_OUTPUT_DIR=./my_results ./scripts/benchmark.sh
```

### Combined Example
```bash
AIKV_REQUESTS=100000 \
AIKV_CLIENTS=100 \
AIKV_PIPELINE=16 \
AIKV_VALUE_SIZE=1024 \
./scripts/benchmark.sh
```

## Understanding Results

### Criterion Output
Criterion provides detailed statistics including:
- **Mean time**: Average execution time
- **Standard deviation**: Variability in measurements
- **Outliers**: Unusual measurements
- **Throughput**: Operations per second

Example output:
```
resp_encoding/simple_string
                        time:   [77.173 ns 77.379 ns 77.584 ns]
```

### Redis-Benchmark Output
The script generates CSV output with:
- Command name
- Requests per second
- Average latency
- Min/max latency
- Percentile latencies (p50, p95, p99)

Results are saved to `benchmark_results/benchmark_TIMESTAMP.txt`

## Performance Baseline

Current baseline results (as of 2025-11-12):

### RESP Operations
- Simple string encoding: ~77 ns
- Integer encoding: ~83 ns
- Bulk string encoding: ~128 ns
- Array encoding: ~425 ns

### Storage Operations
- SET: ~52 ns
- GET: ~45 ns
- EXISTS: ~31 ns
- DELETE: ~121 ns

### Multi-Key Operations
| Operation | 10 Keys | 100 Keys | 1000 Keys |
|-----------|---------|----------|-----------|
| MSET      | 562 ns  | 7.5 µs   | 79 µs     |
| MGET      | 310 ns  | 3.1 µs   | 41 µs     |

## Comparing with Redis

To compare AiKv performance with Redis:

1. Start AiKv on port 6379:
```bash
cargo run --release
```

2. Run benchmarks and save results:
```bash
AIKV_OUTPUT_DIR=./aikv_results ./scripts/benchmark.sh
```

3. Start Redis on a different port:
```bash
redis-server --port 6380
```

4. Run benchmarks against Redis:
```bash
AIKV_PORT=6380 AIKV_OUTPUT_DIR=./redis_results ./scripts/benchmark.sh
```

5. Compare the results in `aikv_results/` and `redis_results/`

## Tips for Accurate Benchmarking

### 1. System Preparation
- Close unnecessary applications
- Disable CPU frequency scaling:
  ```bash
  sudo cpupower frequency-set --governor performance
  ```
- Run benchmarks multiple times and average results

### 2. Server Configuration
- Use release builds: `cargo run --release`
- Ensure adequate system resources
- Monitor system metrics during benchmarks

### 3. Benchmark Configuration
- Start with default parameters
- Gradually increase load (clients, requests, pipeline depth)
- Test with realistic data sizes
- Consider your specific use case

### 4. Result Interpretation
- Focus on median/mean rather than best case
- Check for high standard deviation (indicates inconsistency)
- Look at percentile latencies (p95, p99) for tail latency
- Compare relative performance, not absolute numbers

## Continuous Benchmarking

### In CI/CD
Criterion can detect performance regressions:
```bash
# Baseline
cargo bench --bench aikv_benchmark -- --save-baseline main

# After changes
cargo bench --bench aikv_benchmark -- --baseline main
```

### Performance Monitoring
For production systems:
1. Set up regular benchmark runs
2. Track metrics over time
3. Alert on performance degradation
4. Correlate with code changes

## Troubleshooting

### "Server not running" Error
Ensure AiKv is running before executing `benchmark.sh`:
```bash
cargo run --release
```

### "redis-benchmark not found"
Install Redis tools:
```bash
# Ubuntu/Debian
sudo apt-get install redis-tools

# macOS
brew install redis
```

### Inconsistent Results
- Check system load: `top` or `htop`
- Ensure no other benchmarks are running
- Increase warmup iterations in Criterion benchmarks
- Use longer benchmark duration

### Out of Memory
- Reduce number of concurrent clients
- Reduce value sizes
- Monitor memory usage: `ps aux | grep aikv`

## Advanced Usage

### Custom Benchmark Commands
Modify `scripts/benchmark.sh` to add custom commands or adjust parameters.

### Profile-Guided Optimization
Use profiling tools to identify bottlenecks:
```bash
# Install cargo-flamegraph
cargo install flamegraph

# Profile the server
cargo flamegraph --bin aikv
```

### Memory Profiling
```bash
# Install heaptrack
sudo apt-get install heaptrack

# Profile memory usage
heaptrack cargo run --release
```

## Resources

- [Criterion.rs Documentation](https://bheisler.github.io/criterion.rs/book/)
- [Redis Benchmark Documentation](https://redis.io/docs/management/optimization/benchmarks/)
- [Performance Analysis Guide](../docs/PERFORMANCE.md)

## Contributing

When adding new benchmarks:
1. Follow existing patterns in benchmark files
2. Add documentation in this README
3. Update performance baseline in `docs/PERFORMANCE.md`
4. Ensure benchmarks are deterministic and reproducible

---

**Last Updated**: 2025-11-12
