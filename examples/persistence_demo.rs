// Example: Demonstrating RDB and AOF persistence
// Run with: cargo run --example persistence_demo

use aikv::persistence::{load_aof, load_rdb, save_rdb, AofSyncPolicy, AofWriter};
use bytes::Bytes;
use std::collections::HashMap;
use tempfile::TempDir;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== AiKv Persistence Demo ===\n");

    // Create a temporary directory for our files
    let temp_dir = TempDir::new()?;
    let rdb_path = temp_dir.path().join("demo.rdb");
    let aof_path = temp_dir.path().join("demo.aof");

    // Demo 1: RDB Snapshots
    println!("--- RDB Snapshot Demo ---");
    demo_rdb(&rdb_path)?;
    println!();

    // Demo 2: AOF Logging
    println!("--- AOF Logging Demo ---");
    demo_aof(&aof_path)?;
    println!();

    println!("Demo completed successfully!");
    Ok(())
}

fn demo_rdb(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Create sample database data
    let mut db0 = HashMap::new();
    db0.insert("user:1:name".to_string(), (Bytes::from("Alice"), None));
    db0.insert(
        "user:1:email".to_string(),
        (Bytes::from("alice@example.com"), None),
    );
    db0.insert(
        "session:abc123".to_string(),
        (
            Bytes::from("user_data"),
            Some(9999999999999), // Expiration timestamp
        ),
    );

    let mut db1 = HashMap::new();
    db1.insert("counter".to_string(), (Bytes::from("42"), None));
    db1.insert("flag".to_string(), (Bytes::from("enabled"), None));

    let databases = vec![db0, db1];

    println!("Creating RDB snapshot with {} databases", databases.len());
    println!("  Database 0: {} keys", databases[0].len());
    println!("  Database 1: {} keys", databases[1].len());

    // Save to RDB
    save_rdb(path, &databases)?;
    println!("✓ Saved to RDB file: {}", path.display());

    // Load from RDB
    let loaded = load_rdb(path)?;
    println!("✓ Loaded from RDB file");
    println!("  Loaded {} databases", loaded.len());

    // Verify data
    if let Some(value) = loaded[0].get("user:1:name") {
        println!(
            "  Verified: user:1:name = {}",
            String::from_utf8_lossy(&value.0)
        );
    }

    Ok(())
}

fn demo_aof(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Creating AOF writer with EverySecond sync policy");

    // Create AOF writer
    let writer = AofWriter::new(path, AofSyncPolicy::EverySecond)?;

    // Log some commands
    println!("Logging commands to AOF:");

    let commands = vec![
        vec!["SET", "user:1:name", "Bob"],
        vec!["SET", "user:1:email", "bob@example.com"],
        vec!["INCR", "page_views"],
        vec!["LPUSH", "events", "user_login"],
        vec!["DEL", "old_key"],
    ];

    for cmd in &commands {
        let cmd_strings: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        writer.log_command(&cmd_strings)?;
        println!("  ✓ {}", cmd.join(" "));
    }

    // Flush to ensure all data is written
    writer.flush()?;
    println!("✓ Flushed AOF file: {}", path.display());

    // Load and replay commands
    let loaded_commands = load_aof(path)?;
    println!("✓ Loaded {} commands from AOF", loaded_commands.len());

    println!("Replaying commands:");
    for (i, cmd) in loaded_commands.iter().enumerate() {
        println!("  {}. {}", i + 1, cmd.join(" "));
    }

    Ok(())
}
