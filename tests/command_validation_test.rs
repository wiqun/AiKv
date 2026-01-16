use aikv::command::CommandExecutor;
use aikv::protocol::RespValue;
use aikv::StorageEngine;
use bytes::Bytes;

// 命令验证结果
#[derive(Debug)]
#[allow(dead_code)]
struct CommandValidation {
    command: String,
    category: String,
    status: ValidationStatus,
    error: Option<String>,
}

#[derive(Debug, PartialEq)]
#[allow(dead_code)]
enum ValidationStatus {
    Passed,
    Failed,
    NotImplemented,
}

struct CommandValidator {
    executor: CommandExecutor,
    current_db: usize,
    client_id: usize,
}

impl CommandValidator {
    fn new() -> Self {
        let storage = StorageEngine::new_memory(16);
        let executor = CommandExecutor::new(storage);
        Self {
            executor,
            current_db: 0,
            client_id: 1,
        }
    }

    fn test_ping(&mut self) -> CommandValidation {
        match self
            .executor
            .execute("PING", &[], &mut self.current_db, self.client_id)
        {
            Ok(resp) => {
                if matches!(resp, RespValue::SimpleString(_)) {
                    CommandValidation {
                        command: "PING".to_string(),
                        category: "Protocol".to_string(),
                        status: ValidationStatus::Passed,
                        error: None,
                    }
                } else {
                    CommandValidation {
                        command: "PING".to_string(),
                        category: "Protocol".to_string(),
                        status: ValidationStatus::Failed,
                        error: Some(format!("Unexpected response: {:?}", resp)),
                    }
                }
            }
            Err(e) => CommandValidation {
                command: "PING".to_string(),
                category: "Protocol".to_string(),
                status: ValidationStatus::Failed,
                error: Some(e.to_string()),
            },
        }
    }

    fn test_echo(&mut self) -> CommandValidation {
        let test_message = "Hello AiKv!";
        match self.executor.execute(
            "ECHO",
            &[Bytes::from(test_message)],
            &mut self.current_db,
            self.client_id,
        ) {
            Ok(resp) => {
                if matches!(resp, RespValue::BulkString(Some(_))) {
                    CommandValidation {
                        command: "ECHO".to_string(),
                        category: "Protocol".to_string(),
                        status: ValidationStatus::Passed,
                        error: None,
                    }
                } else {
                    CommandValidation {
                        command: "ECHO".to_string(),
                        category: "Protocol".to_string(),
                        status: ValidationStatus::Failed,
                        error: Some(format!("Unexpected response: {:?}", resp)),
                    }
                }
            }
            Err(e) => CommandValidation {
                command: "ECHO".to_string(),
                category: "Protocol".to_string(),
                status: ValidationStatus::Failed,
                error: Some(e.to_string()),
            },
        }
    }

    fn test_set_get(&mut self) -> CommandValidation {
        let key = "test_key";
        let value = "test_value";

        // Test SET
        match self.executor.execute(
            "SET",
            &[Bytes::from(key), Bytes::from(value)],
            &mut self.current_db,
            self.client_id,
        ) {
            Ok(resp) => {
                if !matches!(resp, RespValue::SimpleString(_)) && resp != RespValue::ok() {
                    return CommandValidation {
                        command: "SET".to_string(),
                        category: "String".to_string(),
                        status: ValidationStatus::Failed,
                        error: Some(format!("SET failed with response: {:?}", resp)),
                    };
                }
            }
            Err(e) => {
                return CommandValidation {
                    command: "SET".to_string(),
                    category: "String".to_string(),
                    status: ValidationStatus::Failed,
                    error: Some(format!("SET failed with error: {}", e)),
                }
            }
        }

        // Test GET
        match self.executor.execute(
            "GET",
            &[Bytes::from(key)],
            &mut self.current_db,
            self.client_id,
        ) {
            Ok(resp) => {
                if matches!(resp, RespValue::BulkString(Some(ref data)) if data == value) {
                    CommandValidation {
                        command: "SET/GET".to_string(),
                        category: "String".to_string(),
                        status: ValidationStatus::Passed,
                        error: None,
                    }
                } else {
                    CommandValidation {
                        command: "SET/GET".to_string(),
                        category: "String".to_string(),
                        status: ValidationStatus::Failed,
                        error: Some(format!("GET returned unexpected value: {:?}", resp)),
                    }
                }
            }
            Err(e) => CommandValidation {
                command: "SET/GET".to_string(),
                category: "String".to_string(),
                status: ValidationStatus::Failed,
                error: Some(format!("GET failed with error: {}", e)),
            },
        }
    }
}

#[test]
fn validate_basic_commands() {
    let mut validator = CommandValidator::new();

    println!("Command Validation Results:");
    println!("==========================");

    // Test PING
    let ping_result = validator.test_ping();
    println!(
        "Protocol - {}: {:?}",
        ping_result.command,
        match ping_result.status {
            ValidationStatus::Passed => "✓ PASSED",
            ValidationStatus::Failed => "✗ FAILED",
            ValidationStatus::NotImplemented => "? NOT IMPLEMENTED",
        }
    );
    if let Some(error) = ping_result.error {
        println!("  Error: {}", error);
    }
    assert_eq!(
        ping_result.status,
        ValidationStatus::Passed,
        "PING command should work"
    );

    // Test ECHO
    let echo_result = validator.test_echo();
    println!(
        "Protocol - {}: {:?}",
        echo_result.command,
        match echo_result.status {
            ValidationStatus::Passed => "✓ PASSED",
            ValidationStatus::Failed => "✗ FAILED",
            ValidationStatus::NotImplemented => "? NOT IMPLEMENTED",
        }
    );
    if let Some(error) = echo_result.error {
        println!("  Error: {}", error);
    }
    assert_eq!(
        echo_result.status,
        ValidationStatus::Passed,
        "ECHO command should work"
    );

    // Test SET/GET
    let setget_result = validator.test_set_get();
    println!(
        "String - {}: {:?}",
        setget_result.command,
        match setget_result.status {
            ValidationStatus::Passed => "✓ PASSED",
            ValidationStatus::Failed => "✗ FAILED",
            ValidationStatus::NotImplemented => "? NOT IMPLEMENTED",
        }
    );
    if let Some(error) = setget_result.error {
        println!("  Error: {}", error);
    }
    assert_eq!(
        setget_result.status,
        ValidationStatus::Passed,
        "SET/GET commands should work"
    );

    println!("\n✓ All basic commands validated successfully!");
}
