use crate::error::{AikvError, Result};
use crate::persistence::config::AofSyncPolicy;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// AOF writer for logging commands
pub struct AofWriter {
    writer: Arc<Mutex<BufWriter<File>>>,
    sync_policy: AofSyncPolicy,
}

impl AofWriter {
    /// Create a new AOF writer
    pub fn new<P: AsRef<Path>>(path: P, sync_policy: AofSyncPolicy) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| AikvError::Persistence(format!("Failed to open AOF file: {}", e)))?;

        Ok(Self {
            writer: Arc::new(Mutex::new(BufWriter::new(file))),
            sync_policy,
        })
    }

    /// Log a command in RESP format
    pub fn log_command(&self, command: &[String]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| AikvError::Persistence(format!("Failed to lock writer: {}", e)))?;

        // Write command in RESP array format
        write!(writer, "*{}\r\n", command.len()).map_err(|e| {
            AikvError::Persistence(format!("Failed to write command length: {}", e))
        })?;

        for arg in command {
            let arg_bytes = arg.as_bytes();
            write!(writer, "${}\r\n", arg_bytes.len()).map_err(|e| {
                AikvError::Persistence(format!("Failed to write arg length: {}", e))
            })?;
            writer
                .write_all(arg_bytes)
                .map_err(|e| AikvError::Persistence(format!("Failed to write arg: {}", e)))?;
            writer
                .write_all(b"\r\n")
                .map_err(|e| AikvError::Persistence(format!("Failed to write CRLF: {}", e)))?;
        }

        // Sync according to policy
        match self.sync_policy {
            AofSyncPolicy::Always => {
                writer
                    .flush()
                    .map_err(|e| AikvError::Persistence(format!("Failed to flush: {}", e)))?;
                writer
                    .get_ref()
                    .sync_all()
                    .map_err(|e| AikvError::Persistence(format!("Failed to sync: {}", e)))?;
            }
            AofSyncPolicy::EverySecond | AofSyncPolicy::No => {
                // For EverySecond, we would need a background thread to sync periodically
                // For No, we let the OS handle it
                // For now, just flush the buffer
                writer
                    .flush()
                    .map_err(|e| AikvError::Persistence(format!("Failed to flush: {}", e)))?;
            }
        }

        Ok(())
    }

    /// Flush the writer
    pub fn flush(&self) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| AikvError::Persistence(format!("Failed to lock writer: {}", e)))?;

        writer
            .flush()
            .map_err(|e| AikvError::Persistence(format!("Failed to flush: {}", e)))?;
        Ok(())
    }
}

impl Clone for AofWriter {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            sync_policy: self.sync_policy,
        }
    }
}

/// AOF reader for replaying commands
pub struct AofReader<R: BufRead> {
    reader: R,
}

impl<R: BufRead> AofReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
        }
    }

    /// Read next command from AOF
    pub fn read_command(&mut self) -> Result<Option<Vec<String>>> {
        let mut line = String::new();

        // Read array length
        match self.reader.read_line(&mut line) {
            Ok(0) => return Ok(None), // EOF
            Ok(_) => {}
            Err(e) => {
                return Err(AikvError::Persistence(format!(
                    "Failed to read line: {}",
                    e
                )))
            }
        }

        if !line.starts_with('*') {
            return Err(AikvError::Persistence(format!(
                "Invalid AOF format: expected array, got: {}",
                line
            )));
        }

        let count = line[1..]
            .trim()
            .parse::<usize>()
            .map_err(|e| AikvError::Persistence(format!("Invalid array length: {}", e)))?;

        let mut command = Vec::with_capacity(count);

        for _ in 0..count {
            line.clear();
            self.reader
                .read_line(&mut line)
                .map_err(|e| AikvError::Persistence(format!("Failed to read line: {}", e)))?;

            if !line.starts_with('$') {
                return Err(AikvError::Persistence(format!(
                    "Invalid AOF format: expected bulk string, got: {}",
                    line
                )));
            }

            let len = line[1..].trim().parse::<usize>().map_err(|e| {
                AikvError::Persistence(format!("Invalid bulk string length: {}", e))
            })?;

            let mut buf = vec![0u8; len];
            self.reader.read_exact(&mut buf).map_err(|e| {
                AikvError::Persistence(format!("Failed to read bulk string: {}", e))
            })?;

            // Read CRLF
            line.clear();
            self.reader
                .read_line(&mut line)
                .map_err(|e| AikvError::Persistence(format!("Failed to read CRLF: {}", e)))?;

            let arg = String::from_utf8(buf)
                .map_err(|e| AikvError::Persistence(format!("Invalid UTF-8: {}", e)))?;
            command.push(arg);
        }

        Ok(Some(command))
    }

    /// Load all commands from AOF
    pub fn load_all(&mut self) -> Result<Vec<Vec<String>>> {
        let mut commands = Vec::new();

        while let Some(command) = self.read_command()? {
            commands.push(command);
        }

        Ok(commands)
    }
}

/// Load commands from AOF file
pub fn load_aof<P: AsRef<Path>>(path: P) -> Result<Vec<Vec<String>>> {
    let file = File::open(path)
        .map_err(|e| AikvError::Persistence(format!("Failed to open AOF file: {}", e)))?;

    let reader = BufReader::new(file);
    let mut aof_reader = AofReader::new(reader);
    aof_reader.load_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::NamedTempFile;

    #[test]
    fn test_aof_write_read() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Write commands
        let writer = AofWriter::new(path, AofSyncPolicy::Always).unwrap();

        writer
            .log_command(&["SET".to_string(), "key1".to_string(), "value1".to_string()])
            .unwrap();

        writer
            .log_command(&["SET".to_string(), "key2".to_string(), "value2".to_string()])
            .unwrap();

        writer
            .log_command(&["DEL".to_string(), "key1".to_string()])
            .unwrap();

        drop(writer);

        // Read commands
        let commands = load_aof(path).unwrap();

        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0], vec!["SET", "key1", "value1"]);
        assert_eq!(commands[1], vec!["SET", "key2", "value2"]);
        assert_eq!(commands[2], vec!["DEL", "key1"]);
    }

    #[test]
    fn test_aof_reader_empty() {
        let cursor = Cursor::new(Vec::new());
        let mut reader = AofReader::new(cursor);

        assert!(reader.read_command().unwrap().is_none());
    }

    #[test]
    fn test_aof_reader_single_command() {
        let data = b"*3\r\n$3\r\nSET\r\n$4\r\nkey1\r\n$6\r\nvalue1\r\n";
        let cursor = Cursor::new(data.to_vec());
        let mut reader = AofReader::new(cursor);

        let command = reader.read_command().unwrap().unwrap();
        assert_eq!(command, vec!["SET", "key1", "value1"]);

        assert!(reader.read_command().unwrap().is_none());
    }

    #[test]
    fn test_aof_reader_multiple_commands() {
        let data =
            b"*3\r\n$3\r\nSET\r\n$4\r\nkey1\r\n$6\r\nvalue1\r\n*2\r\n$3\r\nGET\r\n$4\r\nkey1\r\n";
        let cursor = Cursor::new(data.to_vec());
        let mut reader = AofReader::new(cursor);

        let commands = reader.load_all().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], vec!["SET", "key1", "value1"]);
        assert_eq!(commands[1], vec!["GET", "key1"]);
    }
}
