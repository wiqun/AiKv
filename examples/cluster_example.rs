/// Example demonstrating cluster operations with AiKv
///
/// This example shows how to use cluster-related operations:
/// - CLUSTER INFO
/// - CLUSTER KEYSLOT
/// - Using hash tags for co-located keys
///
/// Note: This example requires a running AiKv server.
/// Cluster features require the 'cluster' feature to be enabled.
///
/// Run the server first:
/// ```
/// cargo run
/// ```
///
/// Then run this example:
/// ```
/// cargo run --example cluster_example
/// ```
use redis::{Commands, Connection};

fn main() -> redis::RedisResult<()> {
    println!("=== AiKv Cluster Operations Example ===\n");

    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con: Connection = client.get_connection()?;

    // Demo 1: CLUSTER KEYSLOT
    println!("--- 1. Key Slot Calculation ---");
    demo_keyslot(&mut con)?;

    // Demo 2: Hash Tags
    println!("\n--- 2. Hash Tags for Key Co-location ---");
    demo_hash_tags(&mut con)?;

    // Demo 3: Key management with patterns
    println!("\n--- 3. Key Management with Patterns ---");
    demo_key_patterns(&mut con)?;

    // Demo 4: Database operations
    println!("\n--- 4. Database Operations ---");
    demo_database_operations(&mut con)?;

    println!("\n✓ All cluster examples completed successfully!");

    Ok(())
}

fn demo_keyslot(con: &mut Connection) -> redis::RedisResult<()> {
    // Calculate slot for different keys
    let keys = ["user:1000", "order:2023", "session:abc", "cache:data"];

    println!("Key slot assignments (16384 slots total):");
    for key in &keys {
        let slot: i32 = redis::cmd("CLUSTER").arg("KEYSLOT").arg(key).query(con)?;
        println!("  {} -> slot {}", key, slot);
    }

    Ok(())
}

fn demo_hash_tags(con: &mut Connection) -> redis::RedisResult<()> {
    // Keys with hash tags will be placed in the same slot
    let tagged_keys = [
        "{user:1000}:profile",
        "{user:1000}:settings",
        "{user:1000}:sessions",
    ];

    println!("Keys with hash tag {{user:1000}}:");
    let mut first_slot = 0;
    for (i, key) in tagged_keys.iter().enumerate() {
        let slot: i32 = redis::cmd("CLUSTER").arg("KEYSLOT").arg(key).query(con)?;
        if i == 0 {
            first_slot = slot;
        }
        let same = if slot == first_slot { "✓" } else { "✗" };
        println!("  {} -> slot {} {}", key, slot, same);
    }

    // Demonstrate that we can use MSET/MGET with tagged keys
    println!("\nUsing MSET with hash-tagged keys:");
    let _: () = redis::cmd("MSET")
        .arg(&[
            "{user:1000}:profile",
            "Alice",
            "{user:1000}:settings",
            "dark_mode",
            "{user:1000}:sessions",
            "3",
        ])
        .query(con)?;
    println!("  MSET succeeded - all keys in same slot");

    let values: Vec<String> = redis::cmd("MGET")
        .arg(&[
            "{user:1000}:profile",
            "{user:1000}:settings",
            "{user:1000}:sessions",
        ])
        .query(con)?;
    println!("  MGET result: {:?}", values);

    // Cleanup
    let _: () = redis::cmd("DEL")
        .arg(&[
            "{user:1000}:profile",
            "{user:1000}:settings",
            "{user:1000}:sessions",
        ])
        .query(con)?;

    Ok(())
}

fn demo_key_patterns(con: &mut Connection) -> redis::RedisResult<()> {
    // Create some test keys
    let _: () = redis::cmd("MSET")
        .arg(&[
            "app:user:1",
            "Alice",
            "app:user:2",
            "Bob",
            "app:order:100",
            "pending",
            "app:order:101",
            "completed",
            "app:cache:home",
            "html_content",
        ])
        .query(con)?;
    println!("Created test keys");

    // KEYS with pattern
    println!("\nKEYS with patterns:");
    let user_keys: Vec<String> = redis::cmd("KEYS").arg("app:user:*").query(con)?;
    println!("  app:user:* -> {:?}", user_keys);

    let order_keys: Vec<String> = redis::cmd("KEYS").arg("app:order:*").query(con)?;
    println!("  app:order:* -> {:?}", order_keys);

    // SCAN (recommended for production)
    println!("\nSCAN with pattern (recommended):");
    let (cursor, keys): (i32, Vec<String>) = redis::cmd("SCAN")
        .arg(0)
        .arg("MATCH")
        .arg("app:*")
        .arg("COUNT")
        .arg(100)
        .query(con)?;
    println!("  SCAN 0 MATCH app:* COUNT 100");
    println!("  Cursor: {}, Keys: {:?}", cursor, keys);

    // TYPE command
    println!("\nTYPE for different keys:");
    con.set::<_, _, ()>("str_key", "hello")?;
    let _: () = redis::cmd("LPUSH")
        .arg("list_key")
        .arg("item1")
        .query(con)?;
    let _: () = redis::cmd("HSET")
        .arg("hash_key")
        .arg("field")
        .arg("value")
        .query(con)?;

    for key in &["str_key", "list_key", "hash_key"] {
        let key_type: String = redis::cmd("TYPE").arg(key).query(con)?;
        println!("  TYPE {} = {}", key, key_type);
    }

    // Cleanup
    let _: () = redis::cmd("DEL")
        .arg(&[
            "app:user:1",
            "app:user:2",
            "app:order:100",
            "app:order:101",
            "app:cache:home",
            "str_key",
            "list_key",
            "hash_key",
        ])
        .query(con)?;

    Ok(())
}

fn demo_database_operations(con: &mut Connection) -> redis::RedisResult<()> {
    // DBSIZE
    let initial_size: i32 = redis::cmd("DBSIZE").query(con)?;
    println!("Initial DBSIZE: {}", initial_size);

    // Create some keys
    let _: () = redis::cmd("MSET")
        .arg(&["test:1", "value1", "test:2", "value2", "test:3", "value3"])
        .query(con)?;

    let size_after: i32 = redis::cmd("DBSIZE").query(con)?;
    println!("After adding 3 keys: DBSIZE = {}", size_after);

    // SELECT (switch database)
    println!("\nDatabase operations:");
    let _: () = redis::cmd("SELECT").arg(1).query(con)?;
    println!("  Switched to database 1");

    let db1_size: i32 = redis::cmd("DBSIZE").query(con)?;
    println!("  Database 1 DBSIZE: {}", db1_size);

    // Switch back to database 0
    let _: () = redis::cmd("SELECT").arg(0).query(con)?;
    println!("  Switched back to database 0");

    // RANDOMKEY
    let random_key: Option<String> = redis::cmd("RANDOMKEY").query(con)?;
    println!("\nRANDOMKEY: {:?}", random_key);

    // EXISTS
    let exists: i32 = redis::cmd("EXISTS")
        .arg(&["test:1", "test:2", "nonexistent"])
        .query(con)?;
    println!("EXISTS test:1 test:2 nonexistent: {} keys exist", exists);

    // Cleanup
    let _: () = redis::cmd("DEL")
        .arg(&["test:1", "test:2", "test:3"])
        .query(con)?;

    Ok(())
}
