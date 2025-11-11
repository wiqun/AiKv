/// Example client demonstrating how to connect to AiKv
///
/// Run the server first:
/// ```
/// cargo run
/// ```
///
/// Then run this example:
/// ```
/// cargo run --example client_example
/// ```
use redis::{Commands, Connection};

fn main() -> redis::RedisResult<()> {
    println!("Connecting to AiKv server at 127.0.0.1:6379...\n");

    // Connect to the server
    let client = redis::Client::open("redis://127.0.0.1:6379")?;
    let mut con: Connection = client.get_connection()?;

    // Test PING
    println!("=== Testing PING ===");
    let pong: String = redis::cmd("PING").query(&mut con)?;
    println!("PING -> {}\n", pong);

    // String operations
    println!("=== Testing String Commands ===");

    // SET and GET
    con.set::<_, _, ()>("mykey", "Hello World")?;
    let value: String = con.get("mykey")?;
    println!("SET mykey 'Hello World'");
    println!("GET mykey -> {}\n", value);

    // MSET and MGET
    let _: () = redis::cmd("MSET")
        .arg("key1")
        .arg("value1")
        .arg("key2")
        .arg("value2")
        .arg("key3")
        .arg("value3")
        .query(&mut con)?;
    println!("MSET key1 value1 key2 value2 key3 value3");

    let values: Vec<String> = redis::cmd("MGET")
        .arg("key1")
        .arg("key2")
        .arg("key3")
        .query(&mut con)?;
    println!("MGET key1 key2 key3 -> {:?}\n", values);

    // EXISTS and DEL
    let exists: i32 = con.exists("key1")?;
    println!("EXISTS key1 -> {}", exists);

    let deleted: i32 = con.del("key1")?;
    println!("DEL key1 -> {} deleted\n", deleted);

    // STRLEN and APPEND
    let len: i32 = redis::cmd("STRLEN").arg("mykey").query(&mut con)?;
    println!("STRLEN mykey -> {}", len);

    let new_len: i32 = redis::cmd("APPEND")
        .arg("mykey")
        .arg("!!!")
        .query(&mut con)?;
    println!("APPEND mykey '!!!' -> new length: {}", new_len);

    let value: String = con.get("mykey")?;
    println!("GET mykey -> {}\n", value);

    // JSON operations
    println!("=== Testing JSON Commands ===");

    // JSON.SET and JSON.GET
    let user_json = r#"{"name":"John Doe","age":30,"email":"john@example.com"}"#;
    let _: () = redis::cmd("JSON.SET")
        .arg("user:1")
        .arg("$")
        .arg(user_json)
        .query(&mut con)?;
    println!("JSON.SET user:1 $ '{}'", user_json);

    let json: String = redis::cmd("JSON.GET").arg("user:1").query(&mut con)?;
    println!("JSON.GET user:1 -> {}\n", json);

    // JSON.TYPE
    let json_type: String = redis::cmd("JSON.TYPE")
        .arg("user:1")
        .arg("$.name")
        .query(&mut con)?;
    println!("JSON.TYPE user:1 $.name -> {}", json_type);

    // JSON.OBJLEN
    let obj_len: i32 = redis::cmd("JSON.OBJLEN").arg("user:1").query(&mut con)?;
    println!("JSON.OBJLEN user:1 -> {} keys\n", obj_len);

    // JSON Array operations
    let array_json = r#"[1,2,3,4,5]"#;
    let _: () = redis::cmd("JSON.SET")
        .arg("numbers")
        .arg("$")
        .arg(array_json)
        .query(&mut con)?;
    println!("JSON.SET numbers $ '{}'", array_json);

    let arr_len: i32 = redis::cmd("JSON.ARRLEN").arg("numbers").query(&mut con)?;
    println!("JSON.ARRLEN numbers -> {}\n", arr_len);

    // Clean up
    println!("=== Cleaning up ===");
    let _: () = con.del(&["mykey", "key2", "key3", "user:1", "numbers"])?;
    println!("Deleted test keys\n");

    println!("✓ All examples completed successfully!");

    Ok(())
}
