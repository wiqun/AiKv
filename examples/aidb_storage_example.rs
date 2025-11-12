// Example demonstrating the use of AiDb storage adapter
//
// This example shows how to use AiKv with the AiDb LSM-Tree storage engine
// for persistent storage.

use aikv::storage::AiDbStorageAdapter;
use bytes::Bytes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== AiKv with AiDb Storage Engine Example ===\n");

    // Create a temporary directory for the database
    let temp_dir = tempfile::tempdir()?;
    let db_path = temp_dir.path();

    println!("Creating AiDb storage adapter at: {:?}\n", db_path);

    // Create AiDb storage adapter with 16 databases (like Redis)
    let storage = AiDbStorageAdapter::new(db_path, 16)?;

    // Basic operations
    println!("1. Basic SET and GET operations:");
    storage.set("user:1".to_string(), Bytes::from("Alice"))?;
    storage.set("user:2".to_string(), Bytes::from("Bob"))?;
    storage.set("user:3".to_string(), Bytes::from("Charlie"))?;

    if let Some(value) = storage.get("user:1")? {
        println!("   user:1 = {}", String::from_utf8_lossy(&value));
    }

    // Multiple databases
    println!("\n2. Multi-database operations:");
    storage.set_in_db(0, "key1".to_string(), Bytes::from("db0-value"))?;
    storage.set_in_db(1, "key1".to_string(), Bytes::from("db1-value"))?;

    if let Some(val0) = storage.get_from_db(0, "key1")? {
        println!("   DB 0, key1 = {}", String::from_utf8_lossy(&val0));
    }
    if let Some(val1) = storage.get_from_db(1, "key1")? {
        println!("   DB 1, key1 = {}", String::from_utf8_lossy(&val1));
    }

    // Batch operations
    println!("\n3. Batch MSET and MGET operations:");
    storage.mset(vec![
        ("product:1".to_string(), Bytes::from("Laptop")),
        ("product:2".to_string(), Bytes::from("Mouse")),
        ("product:3".to_string(), Bytes::from("Keyboard")),
    ])?;

    let products = storage.mget(&[
        "product:1".to_string(),
        "product:2".to_string(),
        "product:3".to_string(),
    ])?;

    for (i, product) in products.iter().enumerate() {
        if let Some(p) = product {
            println!("   product:{} = {}", i + 1, String::from_utf8_lossy(p));
        }
    }

    // Expiration
    println!("\n4. Key expiration (TTL):");
    storage.set("session:123".to_string(), Bytes::from("active"))?;
    storage.set_expire_in_db(0, "session:123", 2000)?; // 2 seconds

    let ttl = storage.get_ttl_in_db(0, "session:123")?;
    println!("   session:123 TTL = {} ms", ttl);

    println!("   Waiting 2.5 seconds for expiration...");
    std::thread::sleep(std::time::Duration::from_millis(2500));

    if storage.get("session:123")?.is_none() {
        println!("   session:123 has expired ✓");
    }

    // Database operations
    println!("\n5. Database operations:");
    let db_size = storage.dbsize_in_db(0)?;
    println!("   Database 0 size: {} keys", db_size);

    let keys = storage.get_all_keys_in_db(0)?;
    println!("   All keys in DB 0: {:?}", keys);

    // Rename operation
    println!("\n6. Rename operation:");
    storage.set("old_key".to_string(), Bytes::from("some_value"))?;
    storage.rename_in_db(0, "old_key", "new_key")?;

    if storage.exists("new_key")? {
        println!("   Key renamed successfully ✓");
        if let Some(value) = storage.get("new_key")? {
            println!("   new_key = {}", String::from_utf8_lossy(&value));
        }
    }

    // Copy operation
    println!("\n7. Copy operation:");
    storage.set("source_key".to_string(), Bytes::from("copy_me"))?;
    storage.copy_in_db(0, 1, "source_key", "dest_key", false)?;

    if let Some(value) = storage.get_from_db(1, "dest_key")? {
        println!(
            "   Copied from DB 0 to DB 1: dest_key = {}",
            String::from_utf8_lossy(&value)
        );
    }

    // Delete operation
    println!("\n8. Delete operation:");
    storage.set("temp_key".to_string(), Bytes::from("temporary"))?;
    println!("   temp_key exists: {}", storage.exists("temp_key")?);

    storage.delete("temp_key")?;
    println!(
        "   After delete, temp_key exists: {}",
        storage.exists("temp_key")?
    );

    println!("\n=== Example completed successfully! ===");
    println!("\nNote: All data was stored persistently in AiDb's LSM-Tree format.");
    println!("The data includes WAL logs and SSTable files for durability.");

    Ok(())
}
