/// Example demonstrating all data types in AiKv
///
/// This example shows how to use different data types:
/// - String
/// - List
/// - Hash
/// - Set
/// - Sorted Set (ZSet)
/// - JSON
///
/// Run the server first:
/// ```
/// cargo run
/// ```
///
/// Then run this example:
/// ```
/// cargo run --example data_types_example
/// ```
use redis::{Commands, Connection};

fn main() -> redis::RedisResult<()> {
    println!("=== AiKv Data Types Example ===\n");

    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con: Connection = client.get_connection()?;

    // Clean up before starting
    cleanup(&mut con);

    // String operations
    println!("--- 1. String Operations ---");
    demo_strings(&mut con)?;

    // List operations
    println!("\n--- 2. List Operations ---");
    demo_lists(&mut con)?;

    // Hash operations
    println!("\n--- 3. Hash Operations ---");
    demo_hashes(&mut con)?;

    // Set operations
    println!("\n--- 4. Set Operations ---");
    demo_sets(&mut con)?;

    // Sorted Set operations
    println!("\n--- 5. Sorted Set Operations ---");
    demo_sorted_sets(&mut con)?;

    // JSON operations
    println!("\n--- 6. JSON Operations ---");
    demo_json(&mut con)?;

    // TTL operations
    println!("\n--- 7. TTL (Expiration) Operations ---");
    demo_ttl(&mut con)?;

    // Clean up
    println!("\n--- Cleaning up ---");
    cleanup(&mut con);

    println!("\n✓ All examples completed successfully!");

    Ok(())
}

fn demo_strings(con: &mut Connection) -> redis::RedisResult<()> {
    // Basic SET and GET
    con.set::<_, _, ()>("greeting", "Hello, AiKv!")?;
    let value: String = con.get("greeting")?;
    println!("SET/GET: greeting = {}", value);

    // APPEND
    let new_len: i32 = redis::cmd("APPEND")
        .arg("greeting")
        .arg(" Welcome!")
        .query(con)?;
    let value: String = con.get("greeting")?;
    println!("APPEND: greeting = {} (length: {})", value, new_len);

    // STRLEN
    let len: i32 = redis::cmd("STRLEN").arg("greeting").query(con)?;
    println!("STRLEN: greeting has {} characters", len);

    // MSET and MGET
    let _: () = redis::cmd("MSET")
        .arg(&["num1", "10", "num2", "20", "num3", "30"])
        .query(con)?;
    let values: Vec<String> = redis::cmd("MGET")
        .arg(&["num1", "num2", "num3"])
        .query(con)?;
    println!("MSET/MGET: {:?}", values);

    Ok(())
}

fn demo_lists(con: &mut Connection) -> redis::RedisResult<()> {
    // LPUSH and RPUSH
    let _: () = redis::cmd("LPUSH")
        .arg("mylist")
        .arg(&["c", "b", "a"])
        .query(con)?;
    let _: () = redis::cmd("RPUSH")
        .arg("mylist")
        .arg(&["d", "e", "f"])
        .query(con)?;

    // LRANGE
    let list: Vec<String> = redis::cmd("LRANGE")
        .arg("mylist")
        .arg(0)
        .arg(-1)
        .query(con)?;
    println!("LRANGE: mylist = {:?}", list);

    // LLEN
    let len: i32 = redis::cmd("LLEN").arg("mylist").query(con)?;
    println!("LLEN: mylist has {} elements", len);

    // LINDEX
    let elem: String = redis::cmd("LINDEX").arg("mylist").arg(2).query(con)?;
    println!("LINDEX: mylist[2] = {}", elem);

    // LPOP and RPOP
    let left: String = redis::cmd("LPOP").arg("mylist").query(con)?;
    let right: String = redis::cmd("RPOP").arg("mylist").query(con)?;
    println!("LPOP: {} | RPOP: {}", left, right);

    Ok(())
}

fn demo_hashes(con: &mut Connection) -> redis::RedisResult<()> {
    // HSET
    let _: () = redis::cmd("HSET")
        .arg("user:1")
        .arg(&["name", "Alice", "email", "alice@example.com", "age", "30"])
        .query(con)?;
    println!("HSET: Created user:1");

    // HGET
    let name: String = redis::cmd("HGET").arg("user:1").arg("name").query(con)?;
    println!("HGET: user:1.name = {}", name);

    // HMGET
    let values: Vec<String> = redis::cmd("HMGET")
        .arg("user:1")
        .arg(&["name", "email", "age"])
        .query(con)?;
    println!("HMGET: user:1 = {:?}", values);

    // HGETALL
    let all: Vec<String> = redis::cmd("HGETALL").arg("user:1").query(con)?;
    println!("HGETALL: user:1 = {:?}", all);

    // HINCRBY
    let new_age: i32 = redis::cmd("HINCRBY")
        .arg("user:1")
        .arg("age")
        .arg(1)
        .query(con)?;
    println!("HINCRBY: user:1.age = {}", new_age);

    // HKEYS and HVALS
    let keys: Vec<String> = redis::cmd("HKEYS").arg("user:1").query(con)?;
    let vals: Vec<String> = redis::cmd("HVALS").arg("user:1").query(con)?;
    println!("HKEYS: {:?} | HVALS: {:?}", keys, vals);

    Ok(())
}

fn demo_sets(con: &mut Connection) -> redis::RedisResult<()> {
    // SADD
    let _: () = redis::cmd("SADD")
        .arg("tags:post:1")
        .arg(&["rust", "programming", "tutorial"])
        .query(con)?;
    let _: () = redis::cmd("SADD")
        .arg("tags:post:2")
        .arg(&["rust", "async", "tokio"])
        .query(con)?;
    println!("SADD: Created two tag sets");

    // SMEMBERS
    let members: Vec<String> = redis::cmd("SMEMBERS").arg("tags:post:1").query(con)?;
    println!("SMEMBERS: tags:post:1 = {:?}", members);

    // SISMEMBER
    let is_member: i32 = redis::cmd("SISMEMBER")
        .arg("tags:post:1")
        .arg("rust")
        .query(con)?;
    println!("SISMEMBER: 'rust' in tags:post:1 = {}", is_member == 1);

    // SCARD
    let count: i32 = redis::cmd("SCARD").arg("tags:post:1").query(con)?;
    println!("SCARD: tags:post:1 has {} members", count);

    // SINTER
    let common: Vec<String> = redis::cmd("SINTER")
        .arg(&["tags:post:1", "tags:post:2"])
        .query(con)?;
    println!("SINTER: Common tags = {:?}", common);

    // SUNION
    let all_tags: Vec<String> = redis::cmd("SUNION")
        .arg(&["tags:post:1", "tags:post:2"])
        .query(con)?;
    println!("SUNION: All tags = {:?}", all_tags);

    // SDIFF
    let diff: Vec<String> = redis::cmd("SDIFF")
        .arg(&["tags:post:1", "tags:post:2"])
        .query(con)?;
    println!("SDIFF: tags:post:1 - tags:post:2 = {:?}", diff);

    Ok(())
}

fn demo_sorted_sets(con: &mut Connection) -> redis::RedisResult<()> {
    // ZADD
    let _: () = redis::cmd("ZADD")
        .arg("leaderboard")
        .arg(&[
            "1000",
            "player:alice",
            "950",
            "player:bob",
            "900",
            "player:charlie",
            "850",
            "player:david",
        ])
        .query(con)?;
    println!("ZADD: Created leaderboard");

    // ZRANGE with scores
    let top: Vec<String> = redis::cmd("ZREVRANGE")
        .arg("leaderboard")
        .arg(0)
        .arg(2)
        .arg("WITHSCORES")
        .query(con)?;
    println!("ZREVRANGE: Top 3 = {:?}", top);

    // ZSCORE
    let score: f64 = redis::cmd("ZSCORE")
        .arg("leaderboard")
        .arg("player:alice")
        .query(con)?;
    println!("ZSCORE: player:alice = {}", score);

    // ZRANK
    let rank: i32 = redis::cmd("ZREVRANK")
        .arg("leaderboard")
        .arg("player:alice")
        .query(con)?;
    println!("ZREVRANK: player:alice is rank #{}", rank + 1);

    // ZINCRBY
    let new_score: f64 = redis::cmd("ZINCRBY")
        .arg("leaderboard")
        .arg(100)
        .arg("player:bob")
        .query(con)?;
    println!("ZINCRBY: player:bob new score = {}", new_score);

    // ZCOUNT
    let count: i32 = redis::cmd("ZCOUNT")
        .arg("leaderboard")
        .arg(900)
        .arg(1100)
        .query(con)?;
    println!("ZCOUNT: {} players with score 900-1100", count);

    Ok(())
}

fn demo_json(con: &mut Connection) -> redis::RedisResult<()> {
    // JSON.SET
    let json_data = r#"{
        "name": "Product",
        "price": 99.99,
        "tags": ["electronics", "sale"],
        "details": {
            "weight": 1.5,
            "dimensions": {"width": 10, "height": 5}
        }
    }"#;
    let _: () = redis::cmd("JSON.SET")
        .arg("product:1")
        .arg("$")
        .arg(json_data)
        .query(con)?;
    println!("JSON.SET: Created product:1");

    // JSON.GET
    let json: String = redis::cmd("JSON.GET").arg("product:1").query(con)?;
    println!("JSON.GET: product:1 = {}", json);

    // JSON.GET with path
    let price: String = redis::cmd("JSON.GET")
        .arg("product:1")
        .arg("$.price")
        .query(con)?;
    println!("JSON.GET $.price = {}", price);

    // JSON.TYPE
    let json_type: String = redis::cmd("JSON.TYPE")
        .arg("product:1")
        .arg("$.tags")
        .query(con)?;
    println!("JSON.TYPE $.tags = {}", json_type);

    // JSON.ARRLEN
    let arr_len: i32 = redis::cmd("JSON.ARRLEN")
        .arg("product:1")
        .arg("$.tags")
        .query(con)?;
    println!("JSON.ARRLEN $.tags = {}", arr_len);

    // JSON.OBJLEN
    let obj_len: i32 = redis::cmd("JSON.OBJLEN")
        .arg("product:1")
        .arg("$.details")
        .query(con)?;
    println!("JSON.OBJLEN $.details = {}", obj_len);

    Ok(())
}

fn demo_ttl(con: &mut Connection) -> redis::RedisResult<()> {
    // SET with EX (seconds)
    let _: () = redis::cmd("SET")
        .arg("session:abc123")
        .arg("user_data")
        .arg("EX")
        .arg(3600)
        .query(con)?;
    println!("SET with EX: session:abc123 expires in 1 hour");

    // TTL
    let ttl: i64 = redis::cmd("TTL").arg("session:abc123").query(con)?;
    println!("TTL: session:abc123 = {} seconds", ttl);

    // EXPIRE
    con.set::<_, _, ()>("temp:key", "value")?;
    let _: i32 = redis::cmd("EXPIRE").arg("temp:key").arg(60).query(con)?;
    println!("EXPIRE: temp:key will expire in 60 seconds");

    // PERSIST
    let _: i32 = redis::cmd("PERSIST").arg("temp:key").query(con)?;
    println!("PERSIST: temp:key TTL removed");

    // Check TTL after PERSIST
    let ttl: i64 = redis::cmd("TTL").arg("temp:key").query(con)?;
    println!("TTL: temp:key = {} (no expiration)", ttl);

    Ok(())
}

fn cleanup(con: &mut Connection) {
    let keys = [
        "greeting",
        "num1",
        "num2",
        "num3",
        "mylist",
        "user:1",
        "tags:post:1",
        "tags:post:2",
        "leaderboard",
        "product:1",
        "session:abc123",
        "temp:key",
    ];
    for key in &keys {
        let _: redis::RedisResult<()> = redis::cmd("DEL").arg(key).query(con);
    }
    println!("Cleaned up test keys");
}
