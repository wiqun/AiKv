use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageEngine, StoredValue};
use bytes::Bytes;
use std::collections::HashSet;

/// Set command handler
pub struct SetCommands {
    storage: StorageEngine,
}

impl SetCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
    }

    /// SADD key member [member ...]
    /// Add the specified members to the set stored at key
    pub fn sadd(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SADD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let set = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();
            let mut count = 0;
            for member in &members {
                if set.insert(member.to_vec()) {
                    count += 1;
                }
            }
            (count, set)
        } else {
            let mut set = HashSet::new();
            let mut count = 0;
            for member in &members {
                if set.insert(member.to_vec()) {
                    count += 1;
                }
            }
            (count, set)
        };

        self.storage
            .set_value(db_index, key, StoredValue::new_set(set.1))?;
        Ok(RespValue::Integer(set.0 as i64))
    }

    /// SREM key member [member ...]
    /// Remove the specified members from the set stored at key
    pub fn srem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let members: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let mut count = 0;

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();

            for member in &members {
                if set.remove(&member.to_vec()) {
                    count += 1;
                }
            }

            // Update or delete the set
            if set.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_set(set))?;
            }
        }

        Ok(RespValue::Integer(count as i64))
    }

    /// SISMEMBER key member
    /// Returns if member is a member of the set stored at key
    pub fn sismember(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("SISMEMBER".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let member = args[1].clone();

        let is_member = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.contains(&member.to_vec())
        } else {
            false
        };

        Ok(RespValue::Integer(if is_member { 1 } else { 0 }))
    }

    /// SMEMBERS key
    /// Returns all the members of the set value stored at key
    pub fn smembers(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SMEMBERS".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.iter().map(|v| Bytes::from(v.clone())).collect()
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            members.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// SCARD key
    /// Returns the set cardinality (number of elements) of the set stored at key
    pub fn scard(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("SCARD".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        let count = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.len()
        } else {
            0
        };

        Ok(RespValue::Integer(count as i64))
    }

    /// SPOP key \[count\]
    /// Remove and return one or multiple random members from the set value stored at key
    pub fn spop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SPOP".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<usize>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        // Migrated: Logic moved from storage layer to command layer
        let mut members = Vec::new();

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut set = stored.as_set()?.clone();

            let to_remove: Vec<Vec<u8>> = set.iter().take(count).cloned().collect();
            for member in to_remove {
                set.remove(&member);
                members.push(Bytes::from(member));
            }

            // Update or delete the set
            if set.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_set(set))?;
            }
        }

        if members.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(members[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                members.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// SRANDMEMBER key \[count\]
    /// Return one or multiple random members from the set value stored at key
    pub fn srandmember(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SRANDMEMBER".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = if args.len() > 1 {
            String::from_utf8_lossy(&args[1])
                .parse::<i64>()
                .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?
        } else {
            1
        };

        let members = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            set.iter()
                .take(count.unsigned_abs() as usize)
                .map(|v| Bytes::from(v.clone()))
                .collect()
        } else {
            Vec::new()
        };

        if members.is_empty() && args.len() == 1 {
            Ok(RespValue::Null)
        } else if count == 1 && args.len() == 1 {
            Ok(RespValue::bulk_string(members[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                members.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// SUNION key [key ...]
    /// Returns the members of the set resulting from the union of all the given sets
    pub fn sunion(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SUNION".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result = HashSet::new();
        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                result.extend(set.iter().cloned());
            }
        }

        Ok(RespValue::Array(Some(
            result
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
        )))
    }

    /// SINTER key [key ...]
    /// Returns the members of the set resulting from the intersection of all the given sets
    pub fn sinter(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SINTER".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result: Option<HashSet<Vec<u8>>> = None;

        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                if let Some(res) = &mut result {
                    *res = res.intersection(set).cloned().collect();
                } else {
                    result = Some(set.clone());
                }
            } else {
                // If any key doesn't exist, intersection is empty
                return Ok(RespValue::Array(Some(Vec::new())));
            }
        }

        Ok(RespValue::Array(Some(
            result
                .unwrap_or_default()
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
        )))
    }

    /// SDIFF key [key ...]
    /// Returns the members of the set resulting from the difference between the first set and all the successive sets
    pub fn sdiff(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("SDIFF".to_string()));
        }

        let keys: Vec<String> = args
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result: HashSet<Vec<u8>> = HashSet::new();

        // Start with the first set
        if let Some(stored) = self.storage.get_value(db_index, &keys[0])? {
            let set = stored.as_set()?;
            result = set.clone();
        }

        // Subtract all other sets
        for key in &keys[1..] {
            if let Some(stored) = self.storage.get_value(db_index, key)? {
                let set = stored.as_set()?;
                result = result.difference(set).cloned().collect();
            }
        }

        Ok(RespValue::Array(Some(
            result
                .into_iter()
                .map(|v| RespValue::bulk_string(Bytes::from(v)))
                .collect(),
        )))
    }

    /// SUNIONSTORE destination key [key ...]
    /// Store the members of the set resulting from the union of all the given sets
    pub fn sunionstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SUNIONSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result = HashSet::new();
        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                result.extend(set.iter().cloned());
            }
        }

        let count = result.len();
        self.storage
            .set_value(db_index, dest, StoredValue::new_set(result))?;

        Ok(RespValue::Integer(count as i64))
    }

    /// SINTERSTORE destination key [key ...]
    /// Store the members of the set resulting from the intersection of all the given sets
    pub fn sinterstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SINTERSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result: Option<HashSet<Vec<u8>>> = None;

        for key in keys {
            if let Some(stored) = self.storage.get_value(db_index, &key)? {
                let set = stored.as_set()?;
                if let Some(res) = &mut result {
                    *res = res.intersection(set).cloned().collect();
                } else {
                    result = Some(set.clone());
                }
            } else {
                // If any key doesn't exist, intersection is empty
                result = Some(HashSet::new());
                break;
            }
        }

        let final_set = result.unwrap_or_default();
        let count = final_set.len();
        self.storage
            .set_value(db_index, dest, StoredValue::new_set(final_set))?;

        Ok(RespValue::Integer(count as i64))
    }

    /// SDIFFSTORE destination key [key ...]
    /// Store the members of the set resulting from the difference between the first set and all the successive sets
    pub fn sdiffstore(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SDIFFSTORE".to_string()));
        }

        let dest = String::from_utf8_lossy(&args[0]).to_string();
        let keys: Vec<String> = args[1..]
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect();

        let mut result: HashSet<Vec<u8>> = HashSet::new();

        // Start with the first set
        if let Some(stored) = self.storage.get_value(db_index, &keys[0])? {
            let set = stored.as_set()?;
            result = set.clone();
        }

        // Subtract all other sets
        for key in &keys[1..] {
            if let Some(stored) = self.storage.get_value(db_index, key)? {
                let set = stored.as_set()?;
                result = result.difference(set).cloned().collect();
            }
        }

        let count = result.len();
        self.storage
            .set_value(db_index, dest, StoredValue::new_set(result))?;

        Ok(RespValue::Integer(count as i64))
    }

    /// SSCAN key cursor [MATCH pattern] [COUNT count]
    /// Incrementally iterates over the members of a set
    pub fn sscan(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("SSCAN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let cursor = String::from_utf8_lossy(&args[1])
            .parse::<usize>()
            .map_err(|_| AikvError::InvalidArgument("ERR invalid cursor".to_string()))?;

        // Parse optional arguments
        let mut pattern: Option<String> = None;
        let mut count: usize = 10; // Default count

        let mut i = 2;
        while i < args.len() {
            let option = String::from_utf8_lossy(&args[i]).to_uppercase();
            match option.as_str() {
                "MATCH" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    pattern = Some(String::from_utf8_lossy(&args[i]).to_string());
                }
                "COUNT" => {
                    if i + 1 >= args.len() {
                        return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                    }
                    i += 1;
                    count = String::from_utf8_lossy(&args[i])
                        .parse::<usize>()
                        .map_err(|_| {
                            AikvError::InvalidArgument(
                                "ERR value is not an integer or out of range".to_string(),
                            )
                        })?;
                }
                _ => {
                    return Err(AikvError::InvalidArgument("ERR syntax error".to_string()));
                }
            }
            i += 1;
        }

        let (next_cursor, members) = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let set = stored.as_set()?;
            let all_members: Vec<Vec<u8>> = set.iter().cloned().collect();

            // Filter by pattern if provided
            let filtered: Vec<Vec<u8>> = if let Some(ref pat) = pattern {
                all_members
                    .into_iter()
                    .filter(|m| {
                        let member_str = String::from_utf8_lossy(m);
                        Self::glob_match(pat, &member_str)
                    })
                    .collect()
            } else {
                all_members
            };

            let total = filtered.len();
            if cursor >= total {
                (0, Vec::new())
            } else {
                let end = (cursor + count).min(total);
                let members: Vec<Vec<u8>> = filtered[cursor..end].to_vec();
                let next = if end >= total { 0 } else { end };
                (next, members)
            }
        } else {
            (0, Vec::new())
        };

        // Build response: [cursor, [members...]]
        let cursor_str = Bytes::from(next_cursor.to_string());
        let members_arr: Vec<RespValue> = members
            .into_iter()
            .map(|m| RespValue::bulk_string(Bytes::from(m)))
            .collect();

        Ok(RespValue::Array(Some(vec![
            RespValue::bulk_string(cursor_str),
            RespValue::Array(Some(members_arr)),
        ])))
    }

    /// SMOVE source destination member
    /// Move a member from one set to another
    pub fn smove(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("SMOVE".to_string()));
        }

        let source_key = String::from_utf8_lossy(&args[0]).to_string();
        let dest_key = String::from_utf8_lossy(&args[1]).to_string();
        let member = args[2].clone();

        // Get source set
        if let Some(stored) = self.storage.get_value(db_index, &source_key)? {
            let mut source_set = stored.as_set()?.clone();

            // Check if member exists in source
            if !source_set.remove(&member.to_vec()) {
                return Ok(RespValue::Integer(0));
            }

            // Get or create destination set and add member
            let dest_set = if source_key == dest_key {
                // Moving within the same set - member was just removed, add it back
                source_set.insert(member.to_vec());
                source_set.clone()
            } else if let Some(dest_stored) = self.storage.get_value(db_index, &dest_key)? {
                let mut dest = dest_stored.as_set()?.clone();
                dest.insert(member.to_vec());
                dest
            } else {
                let mut new_set = HashSet::new();
                new_set.insert(member.to_vec());
                new_set
            };

            // Update source set
            if source_key != dest_key {
                if source_set.is_empty() {
                    self.storage.delete_from_db(db_index, &source_key)?;
                } else {
                    self.storage.set_value(
                        db_index,
                        source_key,
                        StoredValue::new_set(source_set),
                    )?;
                }
            }

            // Update destination set
            self.storage
                .set_value(db_index, dest_key, StoredValue::new_set(dest_set))?;

            Ok(RespValue::Integer(1))
        } else {
            Ok(RespValue::Integer(0))
        }
    }

    /// Simple glob pattern matching for SSCAN
    fn glob_match(pattern: &str, text: &str) -> bool {
        let mut pattern_chars = pattern.chars().peekable();
        let mut text_chars = text.chars().peekable();

        while let Some(p) = pattern_chars.next() {
            match p {
                '*' => {
                    // Skip consecutive stars
                    while pattern_chars.peek() == Some(&'*') {
                        pattern_chars.next();
                    }
                    // If star is at end, match rest of text
                    if pattern_chars.peek().is_none() {
                        return true;
                    }
                    // Try matching remaining pattern at each position
                    let remaining_pattern: String = pattern_chars.collect();
                    while text_chars.peek().is_some() {
                        let remaining_text: String = text_chars.clone().collect();
                        if Self::glob_match(&remaining_pattern, &remaining_text) {
                            return true;
                        }
                        text_chars.next();
                    }
                    return Self::glob_match(&remaining_pattern, "");
                }
                '?' => {
                    if text_chars.next().is_none() {
                        return false;
                    }
                }
                c => {
                    if text_chars.next() != Some(c) {
                        return false;
                    }
                }
            }
        }
        text_chars.peek().is_none()
    }
}
