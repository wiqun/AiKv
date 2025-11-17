/// Example demonstrating Lua script transaction support
///
/// This example shows how Lua scripts in AiKv have transactional semantics:
/// - Successful scripts commit all changes
/// - Failed scripts rollback all changes
/// - Scripts can read their own writes
use aikv::command::script::ScriptCommands;
use aikv::storage::StorageAdapter;
use bytes::Bytes;

fn main() {
    println!("=== Lua Script Transaction Demo ===\n");

    let storage = StorageAdapter::with_db_count(16);
    let script_commands = ScriptCommands::new(storage.clone());

    // Example 1: Successful commit
    println!("Example 1: Successful Commit");
    println!("-------------------------------");
    let script1 = r#"
        redis.call('SET', 'user:1:name', 'Alice')
        redis.call('SET', 'user:1:email', 'alice@example.com')
        return 'User created successfully'
    "#;
    let args1 = vec![Bytes::from(script1), Bytes::from("0")];

    match script_commands.eval(&args1, 0) {
        Ok(result) => println!("Script result: {:?}", result),
        Err(e) => println!("Script failed: {}", e),
    }

    // Verify changes are committed
    let name = storage.get_from_db(0, "user:1:name").unwrap();
    let email = storage.get_from_db(0, "user:1:email").unwrap();
    println!("After success:");
    println!(
        "  user:1:name = {:?}",
        name.map(|b| String::from_utf8_lossy(&b).to_string())
    );
    println!(
        "  user:1:email = {:?}",
        email.map(|b| String::from_utf8_lossy(&b).to_string())
    );
    println!();

    // Example 2: Automatic rollback
    println!("Example 2: Automatic Rollback");
    println!("-------------------------------");
    let script2 = r#"
        redis.call('SET', 'user:2:name', 'Bob')
        redis.call('SET', 'user:2:email', 'bob@example.com')
        error('Validation failed')
    "#;
    let args2 = vec![Bytes::from(script2), Bytes::from("0")];

    match script_commands.eval(&args2, 0) {
        Ok(result) => println!("Script result: {:?}", result),
        Err(e) => println!("Script failed: {}", e),
    }

    // Verify changes are rolled back
    let name = storage.get_from_db(0, "user:2:name").unwrap();
    let email = storage.get_from_db(0, "user:2:email").unwrap();
    println!("After failure:");
    println!(
        "  user:2:name = {:?}",
        name.map(|b| String::from_utf8_lossy(&b).to_string())
    );
    println!(
        "  user:2:email = {:?}",
        email.map(|b| String::from_utf8_lossy(&b).to_string())
    );
    println!();

    // Example 3: Read your own writes
    println!("Example 3: Read Your Own Writes");
    println!("--------------------------------");
    let script3 = r#"
        redis.call('SET', 'counter', '0')
        local v1 = redis.call('GET', 'counter')
        
        redis.call('SET', 'counter', '1')
        local v2 = redis.call('GET', 'counter')
        
        redis.call('SET', 'counter', '2')
        local v3 = redis.call('GET', 'counter')
        
        return {v1, v2, v3}
    "#;
    let args3 = vec![Bytes::from(script3), Bytes::from("0")];

    match script_commands.eval(&args3, 0) {
        Ok(result) => println!("Script result: {:?}", result),
        Err(e) => println!("Script failed: {}", e),
    }

    let final_value = storage.get_from_db(0, "counter").unwrap();
    println!("Final committed value:");
    println!(
        "  counter = {:?}",
        final_value.map(|b| String::from_utf8_lossy(&b).to_string())
    );
    println!();

    // Example 4: Delete and recreate
    println!("Example 4: Delete and Recreate");
    println!("--------------------------------");

    // First set a value
    storage
        .set_in_db(0, "temp_key".to_string(), Bytes::from("old_value"))
        .unwrap();

    let script4 = r#"
        local exists1 = redis.call('EXISTS', 'temp_key')
        redis.call('DEL', 'temp_key')
        local exists2 = redis.call('EXISTS', 'temp_key')
        redis.call('SET', 'temp_key', 'new_value')
        local exists3 = redis.call('EXISTS', 'temp_key')
        local final_val = redis.call('GET', 'temp_key')
        return {exists1, exists2, exists3, final_val}
    "#;
    let args4 = vec![Bytes::from(script4), Bytes::from("0")];

    match script_commands.eval(&args4, 0) {
        Ok(result) => println!("Script result: {:?}", result),
        Err(e) => println!("Script failed: {}", e),
    }

    let final_value = storage.get_from_db(0, "temp_key").unwrap();
    println!("Final committed value:");
    println!(
        "  temp_key = {:?}",
        final_value.map(|b| String::from_utf8_lossy(&b).to_string())
    );

    println!("\n=== Demo Complete ===");
    println!("\nKey Takeaways:");
    println!("1. Successful scripts commit all changes atomically");
    println!("2. Failed scripts rollback all changes automatically");
    println!("3. Scripts can read their own uncommitted writes");
    println!("4. Operations within a script are isolated until commit");
}
