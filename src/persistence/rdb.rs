use crate::error::{AikvError, Result};
use crate::storage::{SerializableStoredValue, StoredValue};
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

/// Save StoredValue database to RDB file
/// This converts StoredValue data to DatabaseData format for RDB compatibility
pub fn save_stored_value_rdb<P: AsRef<Path>>(path: P, databases: &[HashMap<String, StoredValue>]) -> Result<()> {
    // Convert StoredValue databases to DatabaseData format
    let rdb_databases: Result<Vec<DatabaseData>> = databases.iter().enumerate().map(|(db_index, db)| {
        let mut rdb_db = HashMap::new();
        for (key, stored_value) in db {
            if !stored_value.is_expired() {
                // For RDB compatibility, we serialize the StoredValue using bincode
                // This allows us to store complex data types in RDB format
                let serialized = bincode::serialize(&stored_value.to_serializable())
                    .map_err(|e| AikvError::Persistence(format!("Failed to serialize value: {}", e)))?;
                rdb_db.insert(key.clone(), (Bytes::from(serialized), stored_value.expires_at()));
            }
        }
        Ok(rdb_db)
    }).collect();

    let rdb_databases = rdb_databases?;
    save_rdb(path, &rdb_databases)
}

/// Load database from RDB file
pub fn load_rdb<P: AsRef<Path>>(path: P) -> Result<Vec<DatabaseData>> {
    let file = File::open(path)
        .map_err(|e| AikvError::Persistence(format!("Failed to open RDB file: {}", e)))?;

    let mut reader = RdbReader::new(file);
    reader.load()
}

/// Load StoredValue database from RDB file
/// This converts DatabaseData format back to StoredValue format
pub fn load_stored_value_rdb<P: AsRef<Path>>(path: P) -> Result<Vec<HashMap<String, StoredValue>>> {
    let rdb_databases = load_rdb(path)?;

    // Convert DatabaseData back to StoredValue format
    let stored_databases: Vec<HashMap<String, StoredValue>> = rdb_databases.into_iter().map(|rdb_db| {
        let mut stored_db = HashMap::new();
        for (key, (data, expire_ms)) in rdb_db {
            // Try to deserialize as StoredValue first (new format)
            match bincode::deserialize::<SerializableStoredValue>(&data) {
                Ok(serializable) => {
                    let mut stored_value = StoredValue::from_serializable(serializable);
                    stored_value.set_expiration(expire_ms);
                    stored_db.insert(key, stored_value);
                }
                Err(_) => {
                    // Fall back to treating as raw bytes (legacy string-only format)
                    let mut stored_value = StoredValue::new_string(data);
                    stored_value.set_expiration(expire_ms);
                    stored_db.insert(key, stored_value);
                }
            }
        }
        stored_db
    }).collect();

    Ok(stored_databases)
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
    fn test_rdb_stored_value_roundtrip() {
        use crate::storage::StoredValue;
        use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Create test data with complex types
        let mut db0 = HashMap::new();

        // String value
        let string_value = StoredValue::new_string(Bytes::from("hello world"));
        db0.insert("string_key".to_string(), string_value);

        // List value
        let mut list = VecDeque::new();
        list.push_back(Bytes::from("item1"));
        list.push_back(Bytes::from("item2"));
        let list_value = StoredValue::new_list(list);
        db0.insert("list_key".to_string(), list_value);

        // Hash value
        let mut hash = HashMap::new();
        hash.insert("field1".to_string(), Bytes::from("value1"));
        hash.insert("field2".to_string(), Bytes::from("value2"));
        let hash_value = StoredValue::new_hash(hash);
        db0.insert("hash_key".to_string(), hash_value);

        // Set value
        let mut set = HashSet::new();
        set.insert(b"member1".to_vec());
        set.insert(b"member2".to_vec());
        let set_value = StoredValue::new_set(set);
        db0.insert("set_key".to_string(), set_value);

        // ZSet value
        let mut zset = BTreeMap::new();
        zset.insert(b"member1".to_vec(), 1.0);
        zset.insert(b"member2".to_vec(), 2.5);
        let zset_value = StoredValue::new_zset(zset);
        db0.insert("zset_key".to_string(), zset_value);

        let databases = vec![db0];

        // Save StoredValue databases
        save_stored_value_rdb(path, &databases).unwrap();

        // Load StoredValue databases
        let loaded = load_stored_value_rdb(path).unwrap();

        // Verify we loaded the correct data
        assert_eq!(loaded.len(), 16); // RDB always returns 16 databases
        let loaded_db = &loaded[0]; // Check the first database
        assert_eq!(loaded_db.len(), 5);

        // Check string value
        let string_val = loaded_db.get("string_key").unwrap();
        assert_eq!(string_val.as_string().unwrap(), &Bytes::from("hello world"));

        // Check list value
        let list_val = loaded_db.get("list_key").unwrap();
        let list_data = list_val.as_list().unwrap();
        assert_eq!(list_data.len(), 2);
        assert_eq!(list_data[0], Bytes::from("item1"));
        assert_eq!(list_data[1], Bytes::from("item2"));

        // Check hash value
        let hash_val = loaded_db.get("hash_key").unwrap();
        let hash_data = hash_val.as_hash().unwrap();
        assert_eq!(hash_data.len(), 2);
        assert_eq!(hash_data.get("field1").unwrap(), &Bytes::from("value1"));
        assert_eq!(hash_data.get("field2").unwrap(), &Bytes::from("value2"));

        // Check set value
        let set_val = loaded_db.get("set_key").unwrap();
        let set_data = set_val.as_set().unwrap();
        assert_eq!(set_data.len(), 2);
        assert!(set_data.contains(&b"member1".to_vec()));
        assert!(set_data.contains(&b"member2".to_vec()));

        // Check zset value
        let zset_val = loaded_db.get("zset_key").unwrap();
        let zset_data = zset_val.as_zset().unwrap();
        assert_eq!(zset_data.len(), 2);
        assert_eq!(zset_data.get(&b"member1".to_vec()).unwrap(), &1.0);
        assert_eq!(zset_data.get(&b"member2".to_vec()).unwrap(), &2.5);
    }
}
