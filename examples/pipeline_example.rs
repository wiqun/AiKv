/// Example demonstrating pipeline operations for better performance
///
/// This example shows how to use Redis pipelining to reduce
/// network round-trips and improve throughput.
///
/// Run the server first:
/// ```
/// cargo run
/// ```
///
/// Then run this example:
/// ```
/// cargo run --example pipeline_example
/// ```
use redis::{Commands, Connection};
use std::time::Instant;

fn main() -> redis::RedisResult<()> {
    println!("=== AiKv Pipeline Performance Example ===\n");

    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con: Connection = client.get_connection()?;

    // Clean up
    cleanup(&mut con);

    // Demo 1: Compare individual vs pipeline operations
    println!("--- 1. Individual vs Pipeline Comparison ---");
    demo_pipeline_comparison(&mut con)?;

    // Demo 2: Batch insert with pipeline
    println!("\n--- 2. Batch Insert with Pipeline ---");
    demo_batch_insert(&mut con)?;

    // Demo 3: Mixed operations in pipeline
    println!("\n--- 3. Mixed Operations in Pipeline ---");
    demo_mixed_operations(&mut con)?;

    // Demo 4: Atomic counter updates
    println!("\n--- 4. Atomic Counter Updates ---");
    demo_atomic_counters(&mut con)?;

    // Clean up
    cleanup(&mut con);

    println!("\n✓ All pipeline examples completed successfully!");

    Ok(())
}

fn demo_pipeline_comparison(con: &mut Connection) -> redis::RedisResult<()> {
    let num_operations = 100;

    // Individual operations
    println!("Performing {} individual SET operations...", num_operations);
    let start = Instant::now();
    for i in 0..num_operations {
        con.set::<_, _, ()>(format!("individual:{}", i), format!("value{}", i))?;
    }
    let individual_time = start.elapsed();
    println!("  Individual: {:?}", individual_time);

    // Pipeline operations
    println!("Performing {} pipeline SET operations...", num_operations);
    let start = Instant::now();
    let mut pipe = redis::pipe();
    for i in 0..num_operations {
        pipe.set(format!("pipeline:{}", i), format!("value{}", i))
            .ignore();
    }
    pipe.query::<()>(con)?;
    let pipeline_time = start.elapsed();
    println!("  Pipeline: {:?}", pipeline_time);

    // Calculate speedup
    let speedup = individual_time.as_micros() as f64 / pipeline_time.as_micros().max(1) as f64;
    println!("\n  Speedup: {:.1}x faster with pipeline!", speedup);

    Ok(())
}

fn demo_batch_insert(con: &mut Connection) -> redis::RedisResult<()> {
    // Simulate user data insertion
    let users = vec![
        ("user:1001", "name", "Alice", "email", "alice@example.com"),
        ("user:1002", "name", "Bob", "email", "bob@example.com"),
        (
            "user:1003",
            "name",
            "Charlie",
            "email",
            "charlie@example.com",
        ),
        ("user:1004", "name", "Diana", "email", "diana@example.com"),
        ("user:1005", "name", "Eve", "email", "eve@example.com"),
    ];

    // Create pipeline for batch insert
    let mut pipe = redis::pipe();
    for (key, field1, value1, field2, value2) in &users {
        pipe.cmd("HSET")
            .arg(*key)
            .arg(*field1)
            .arg(*value1)
            .arg(*field2)
            .arg(*value2)
            .ignore();
    }

    println!("Inserting {} user records with pipeline...", users.len());
    let start = Instant::now();
    pipe.query::<()>(con)?;
    println!("  Completed in {:?}", start.elapsed());

    // Verify with pipeline read
    let mut read_pipe = redis::pipe();
    for (key, _, _, _, _) in &users {
        read_pipe.cmd("HGETALL").arg(*key);
    }

    let results: Vec<Vec<String>> = read_pipe.query(con)?;
    println!("  Verified {} records:", results.len());
    for (i, result) in results.iter().enumerate() {
        println!("    user:{:04}: {:?}", 1001 + i, result);
    }

    Ok(())
}

fn demo_mixed_operations(con: &mut Connection) -> redis::RedisResult<()> {
    // Setup initial data
    con.set::<_, _, ()>("counter", "0")?;
    let _: () = redis::cmd("LPUSH").arg("events").arg("init").query(con)?;

    println!("Executing mixed operations in single pipeline...");

    // Create pipeline with different operation types
    let mut pipe = redis::pipe();

    // String operations
    pipe.cmd("SET").arg("status").arg("active").ignore();
    pipe.cmd("GET").arg("counter");
    pipe.cmd("INCR").arg("counter");

    // List operations
    pipe.cmd("LPUSH").arg("events").arg("event1").ignore();
    pipe.cmd("LPUSH").arg("events").arg("event2").ignore();
    pipe.cmd("LLEN").arg("events");

    // Hash operations
    pipe.cmd("HSET")
        .arg("stats")
        .arg("requests")
        .arg("100")
        .ignore();
    pipe.cmd("HGET").arg("stats").arg("requests");

    // Execute pipeline and get results
    let results: Vec<redis::Value> = pipe.query(con)?;

    println!("  Pipeline results:");
    println!("    GET counter (before INCR): {:?}", results[0]);
    println!("    INCR counter: {:?}", results[1]);
    println!("    LLEN events: {:?}", results[2]);
    println!("    HGET stats.requests: {:?}", results[3]);

    Ok(())
}

fn demo_atomic_counters(con: &mut Connection) -> redis::RedisResult<()> {
    // Simulate analytics counters
    let events = vec![
        ("page_view", 10),
        ("click", 5),
        ("purchase", 2),
        ("signup", 1),
    ];

    println!("Updating multiple counters atomically...");

    // Initialize counters
    let mut init_pipe = redis::pipe();
    for (event, _) in &events {
        init_pipe
            .cmd("SET")
            .arg(format!("counter:{}", event))
            .arg("0")
            .ignore();
    }
    init_pipe.query::<()>(con)?;

    // Simulate batch increment
    let mut incr_pipe = redis::pipe();
    for (event, count) in &events {
        incr_pipe
            .cmd("INCRBY")
            .arg(format!("counter:{}", event))
            .arg(*count);
    }

    let results: Vec<i64> = incr_pipe.query(con)?;
    println!("  Counter updates:");
    for ((event, increment), result) in events.iter().zip(results.iter()) {
        println!("    counter:{} += {} -> {}", event, increment, result);
    }

    // Read all counters at once
    let mut read_pipe = redis::pipe();
    for (event, _) in &events {
        read_pipe.cmd("GET").arg(format!("counter:{}", event));
    }

    let final_values: Vec<String> = read_pipe.query(con)?;
    println!("\n  Final counter values:");
    for ((event, _), value) in events.iter().zip(final_values.iter()) {
        println!("    counter:{} = {}", event, value);
    }

    Ok(())
}

fn cleanup(con: &mut Connection) {
    // Clean up test keys
    let patterns = [
        "individual:*",
        "pipeline:*",
        "user:100*",
        "counter*",
        "events",
        "status",
        "stats",
    ];

    for pattern in &patterns {
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(pattern)
            .query(con)
            .unwrap_or_default();
        if !keys.is_empty() {
            let _: redis::RedisResult<()> = redis::cmd("DEL").arg(&keys).query(con);
        }
    }
}
