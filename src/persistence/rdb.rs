use crate::error::{AikvError, Result};
use bytes::Bytes;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// RDB magic string "REDIS"
const RDB_MAGIC: &[u8] = b"REDIS";

/// Type alias for database data structure
pub type DatabaseData = HashMap<String, (Bytes, Option<u64>)>;

/// Opcodes for RDB file format
const OPCODE_EOF: u8 = 0xFF;
const OPCODE_SELECTDB: u8 = 0xFE;
const OPCODE_EXPIRETIME_MS: u8 = 0xFC;
const OPCODE_AUX: u8 = 0xFA;

/// RDB writer for creating database snapshots
pub struct RdbWriter<W: Write> {
    writer: BufWriter<W>,
}

impl<W: Write> RdbWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer: BufWriter::new(writer),
        }
    }

    /// Write RDB header
    fn write_header(&mut self) -> Result<()> {
        self.writer
            .write_all(RDB_MAGIC)
            .map_err(|e| AikvError::Persistence(format!("Failed to write magic: {}", e)))?;
        self.writer
            .write_all(b"0001")
            .map_err(|e| AikvError::Persistence(format!("Failed to write version: {}", e)))?;
        Ok(())
    }

    /// Write auxiliary field
    fn write_aux(&mut self, key: &str, value: &str) -> Result<()> {
        self.writer
            .write_all(&[OPCODE_AUX])
            .map_err(|e| AikvError::Persistence(format!("Failed to write aux opcode: {}", e)))?;
        self.write_string(key)?;
        self.write_string(value)?;
        Ok(())
    }

    /// Write string
    fn write_string(&mut self, s: &str) -> Result<()> {
        let bytes = s.as_bytes();
        self.write_length(bytes.len())?;
        self.writer
            .write_all(bytes)
            .map_err(|e| AikvError::Persistence(format!("Failed to write string: {}", e)))?;
        Ok(())
    }

    /// Write length encoding
    fn write_length(&mut self, len: usize) -> Result<()> {
        if len < 64 {
            // 6-bit length
            self.writer
                .write_all(&[(len as u8) & 0x3F])
                .map_err(|e| AikvError::Persistence(format!("Failed to write length: {}", e)))?;
        } else if len < 16384 {
            // 14-bit length
            self.writer
                .write_all(&[0x40 | ((len >> 8) as u8), (len & 0xFF) as u8])
                .map_err(|e| AikvError::Persistence(format!("Failed to write length: {}", e)))?;
        } else {
            // 32-bit length
            self.writer
                .write_all(&[0x80])
                .map_err(|e| AikvError::Persistence(format!("Failed to write length: {}", e)))?;
            self.writer
                .write_all(&(len as u32).to_be_bytes())
                .map_err(|e| AikvError::Persistence(format!("Failed to write length: {}", e)))?;
        }
        Ok(())
    }

    /// Write database selector
    fn write_select_db(&mut self, db_index: usize) -> Result<()> {
        self.writer
            .write_all(&[OPCODE_SELECTDB])
            .map_err(|e| AikvError::Persistence(format!("Failed to write selectdb: {}", e)))?;
        self.write_length(db_index)?;
        Ok(())
    }

    /// Write a key-value pair
    fn write_key_value(&mut self, key: &str, value: &[u8], expire_ms: Option<u64>) -> Result<()> {
        // Write expiration if present
        if let Some(expire_at) = expire_ms {
            self.writer
                .write_all(&[OPCODE_EXPIRETIME_MS])
                .map_err(|e| AikvError::Persistence(format!("Failed to write expire: {}", e)))?;
            self.writer
                .write_all(&expire_at.to_le_bytes())
                .map_err(|e| {
                    AikvError::Persistence(format!("Failed to write expire time: {}", e))
                })?;
        }

        // Write value type (0 = string)
        self.writer
            .write_all(&[0])
            .map_err(|e| AikvError::Persistence(format!("Failed to write type: {}", e)))?;

        // Write key
        self.write_string(key)?;

        // Write value
        self.write_length(value.len())?;
        self.writer
            .write_all(value)
            .map_err(|e| AikvError::Persistence(format!("Failed to write value: {}", e)))?;

        Ok(())
    }

    /// Write database snapshot
    pub fn write_database(&mut self, db_index: usize, data: &DatabaseData) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.write_select_db(db_index)?;

        for (key, (value, expire_ms)) in data {
            self.write_key_value(key, value, *expire_ms)?;
        }

        Ok(())
    }

    /// Finish writing and add EOF marker with checksum
    pub fn finish(mut self) -> Result<()> {
        self.writer
            .write_all(&[OPCODE_EOF])
            .map_err(|e| AikvError::Persistence(format!("Failed to write EOF: {}", e)))?;

        // Write 8-byte checksum (simplified - just write zeros for now)
        self.writer
            .write_all(&[0u8; 8])
            .map_err(|e| AikvError::Persistence(format!("Failed to write checksum: {}", e)))?;

        self.writer
            .flush()
            .map_err(|e| AikvError::Persistence(format!("Failed to flush: {}", e)))?;

        Ok(())
    }
}

/// RDB reader for loading database snapshots
pub struct RdbReader<R: Read> {
    reader: BufReader<R>,
}

impl<R: Read> RdbReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }

    /// Read and verify RDB header
    fn read_header(&mut self) -> Result<u16> {
        let mut magic = [0u8; 5];
        self.reader
            .read_exact(&mut magic)
            .map_err(|e| AikvError::Persistence(format!("Failed to read magic: {}", e)))?;

        if magic != RDB_MAGIC {
            return Err(AikvError::Persistence("Invalid RDB magic".to_string()));
        }

        let mut version = [0u8; 4];
        self.reader
            .read_exact(&mut version)
            .map_err(|e| AikvError::Persistence(format!("Failed to read version: {}", e)))?;

        let version_str = std::str::from_utf8(&version)
            .map_err(|e| AikvError::Persistence(format!("Invalid version string: {}", e)))?;
        let version_num = version_str
            .parse::<u16>()
            .map_err(|e| AikvError::Persistence(format!("Invalid version number: {}", e)))?;

        Ok(version_num)
    }

    /// Read length encoding
    fn read_length(&mut self) -> Result<usize> {
        let mut first_byte = [0u8; 1];
        self.reader
            .read_exact(&mut first_byte)
            .map_err(|e| AikvError::Persistence(format!("Failed to read length: {}", e)))?;

        let first = first_byte[0];
        let encoding = (first & 0xC0) >> 6;

        match encoding {
            0 => Ok((first & 0x3F) as usize),
            1 => {
                let mut second_byte = [0u8; 1];
                self.reader
                    .read_exact(&mut second_byte)
                    .map_err(|e| AikvError::Persistence(format!("Failed to read length: {}", e)))?;
                Ok((((first & 0x3F) as usize) << 8) | (second_byte[0] as usize))
            }
            2 => {
                let mut len_bytes = [0u8; 4];
                self.reader
                    .read_exact(&mut len_bytes)
                    .map_err(|e| AikvError::Persistence(format!("Failed to read length: {}", e)))?;
                Ok(u32::from_be_bytes(len_bytes) as usize)
            }
            _ => Err(AikvError::Persistence(
                "Invalid length encoding".to_string(),
            )),
        }
    }

    /// Read string
    fn read_string(&mut self) -> Result<String> {
        let len = self.read_length()?;
        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .map_err(|e| AikvError::Persistence(format!("Failed to read string: {}", e)))?;
        String::from_utf8(buf)
            .map_err(|e| AikvError::Persistence(format!("Invalid UTF-8 string: {}", e)))
    }

    /// Read bytes
    fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.reader
            .read_exact(&mut buf)
            .map_err(|e| AikvError::Persistence(format!("Failed to read bytes: {}", e)))?;
        Ok(buf)
    }

    /// Load database from RDB file
    pub fn load(&mut self) -> Result<Vec<DatabaseData>> {
        let _version = self.read_header()?;

        let mut databases: Vec<DatabaseData> = vec![HashMap::new(); 16];
        let mut current_db = 0;
        let mut expire_ms: Option<u64> = None;

        loop {
            let mut opcode = [0u8; 1];
            if self.reader.read_exact(&mut opcode).is_err() {
                break;
            }

            match opcode[0] {
                OPCODE_EOF => {
                    // Read checksum
                    let mut _checksum = [0u8; 8];
                    let _ = self.reader.read_exact(&mut _checksum);
                    break;
                }
                OPCODE_SELECTDB => {
                    current_db = self.read_length()?;
                    if current_db >= databases.len() {
                        databases.resize(current_db + 1, HashMap::new());
                    }
                }
                OPCODE_EXPIRETIME_MS => {
                    let mut time_bytes = [0u8; 8];
                    self.reader.read_exact(&mut time_bytes).map_err(|e| {
                        AikvError::Persistence(format!("Failed to read expiretime: {}", e))
                    })?;
                    expire_ms = Some(u64::from_le_bytes(time_bytes));
                }
                OPCODE_AUX => {
                    // Skip auxiliary fields
                    let _key = self.read_string()?;
                    let _value = self.read_string()?;
                }
                // Value type (0 = string)
                0 => {
                    let key = self.read_string()?;
                    let value_len = self.read_length()?;
                    let value = self.read_bytes(value_len)?;

                    databases[current_db].insert(key, (Bytes::from(value), expire_ms));
                    expire_ms = None;
                }
                _ => {
                    return Err(AikvError::Persistence(format!(
                        "Unknown opcode: {}",
                        opcode[0]
                    )));
                }
            }
        }

        Ok(databases)
    }
}

/// Save database to RDB file
pub fn save_rdb<P: AsRef<Path>>(path: P, databases: &[DatabaseData]) -> Result<()> {
    let file = File::create(path)
        .map_err(|e| AikvError::Persistence(format!("Failed to create RDB file: {}", e)))?;

    let mut writer = RdbWriter::new(file);
    writer.write_header()?;

    // Write metadata
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    writer.write_aux("ctime", &now.to_string())?;
    writer.write_aux("aikv-ver", env!("CARGO_PKG_VERSION"))?;

    // Write each database
    for (db_index, db_data) in databases.iter().enumerate() {
        writer.write_database(db_index, db_data)?;
    }

    writer.finish()?;
    Ok(())
}

/// Load database from RDB file
pub fn load_rdb<P: AsRef<Path>>(path: P) -> Result<Vec<DatabaseData>> {
    let file = File::open(path)
        .map_err(|e| AikvError::Persistence(format!("Failed to open RDB file: {}", e)))?;

    let mut reader = RdbReader::new(file);
    reader.load()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::NamedTempFile;

    #[test]
    fn test_rdb_write_read() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Create test data
        let mut db0 = HashMap::new();
        db0.insert("key1".to_string(), (Bytes::from("value1"), None));
        db0.insert(
            "key2".to_string(),
            (Bytes::from("value2"), Some(9999999999999)),
        );

        let mut db1 = HashMap::new();
        db1.insert("key3".to_string(), (Bytes::from("value3"), None));

        let databases = vec![db0, db1];

        // Save
        save_rdb(path, &databases).unwrap();

        // Load
        let loaded = load_rdb(path).unwrap();

        // Check that we loaded the correct databases (we may have extra empty ones)
        assert!(loaded.len() >= databases.len());
        assert_eq!(loaded[0].len(), 2);
        assert_eq!(loaded[1].len(), 1);

        assert_eq!(loaded[0].get("key1").unwrap().0, Bytes::from("value1"));
        assert_eq!(loaded[0].get("key2").unwrap().0, Bytes::from("value2"));
        assert_eq!(loaded[1].get("key3").unwrap().0, Bytes::from("value3"));
    }

    #[test]
    fn test_rdb_length_encoding() {
        let mut cursor = Cursor::new(Vec::new());
        let mut writer = RdbWriter::new(&mut cursor);

        // Test 6-bit length
        writer.write_length(10).unwrap();
        // Test 14-bit length
        writer.write_length(1000).unwrap();
        // Test 32-bit length
        writer.write_length(100000).unwrap();

        drop(writer);

        let data = cursor.into_inner();
        let mut reader = RdbReader::new(Cursor::new(data));

        assert_eq!(reader.read_length().unwrap(), 10);
        assert_eq!(reader.read_length().unwrap(), 1000);
        assert_eq!(reader.read_length().unwrap(), 100000);
    }
}
