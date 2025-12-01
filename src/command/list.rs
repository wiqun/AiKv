use crate::error::{AikvError, Result};
use crate::protocol::RespValue;
use crate::storage::{StorageEngine, StoredValue};
use bytes::Bytes;
use std::collections::VecDeque;

/// List command handler
pub struct ListCommands {
    storage: StorageEngine,
}

impl ListCommands {
    pub fn new(storage: StorageEngine) -> Self {
        Self {
            storage,
        }
    }

    /// LPUSH key element [element ...]
    /// Insert all the specified values at the head of the list stored at key
    pub fn lpush(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("LPUSH".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let elements: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let list = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            // Get existing list or return error if wrong type
            let mut list = stored.as_list()?.clone();
            // Insert elements at the front (left) in reverse order to maintain order
            for element in elements.iter().rev() {
                list.push_front(element.clone());
            }
            list
        } else {
            // Create new list with elements
            let mut list = VecDeque::new();
            for element in elements.iter().rev() {
                list.push_front(element.clone());
            }
            list
        };

        let len = list.len();
        self.storage
            .set_value(db_index, key, StoredValue::new_list(list))?;
        Ok(RespValue::Integer(len as i64))
    }

    /// RPUSH key element [element ...]
    /// Insert all the specified values at the tail of the list stored at key
    pub fn rpush(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() < 2 {
            return Err(AikvError::WrongArgCount("RPUSH".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let elements: Vec<Bytes> = args[1..].to_vec();

        // Migrated: Logic moved from storage layer to command layer
        let list = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            // Get existing list or return error if wrong type
            let mut list = stored.as_list()?.clone();
            // Insert elements at the back (right)
            for element in elements {
                list.push_back(element);
            }
            list
        } else {
            // Create new list with elements
            let mut list = VecDeque::new();
            for element in elements {
                list.push_back(element);
            }
            list
        };

        let len = list.len();
        self.storage
            .set_value(db_index, key, StoredValue::new_list(list))?;
        Ok(RespValue::Integer(len as i64))
    }

    /// LPOP key \[count\]
    /// Remove and return the first elements of the list stored at key
    pub fn lpop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("LPOP".to_string()));
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
        let mut values = Vec::new();

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut list = stored.as_list()?.clone();

            // Pop elements from the front
            for _ in 0..count.min(list.len()) {
                if let Some(value) = list.pop_front() {
                    values.push(value);
                }
            }

            // Update or delete the list
            if list.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_list(list))?;
            }
        }

        if values.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(values[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                values.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// RPOP key \[count\]
    /// Remove and return the last elements of the list stored at key
    pub fn rpop(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.is_empty() {
            return Err(AikvError::WrongArgCount("RPOP".to_string()));
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
        let mut values = Vec::new();

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut list = stored.as_list()?.clone();

            // Pop elements from the back
            for _ in 0..count.min(list.len()) {
                if let Some(value) = list.pop_back() {
                    values.push(value);
                }
            }

            // Update or delete the list
            if list.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_list(list))?;
            }
        }

        if values.is_empty() {
            Ok(RespValue::Null)
        } else if count == 1 {
            Ok(RespValue::bulk_string(values[0].clone()))
        } else {
            Ok(RespValue::Array(Some(
                values.into_iter().map(RespValue::bulk_string).collect(),
            )))
        }
    }

    /// LLEN key
    /// Returns the length of the list stored at key
    pub fn llen(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 1 {
            return Err(AikvError::WrongArgCount("LLEN".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();

        // Migrated: Logic moved from storage layer to command layer
        let len = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            stored.as_list()?.len()
        } else {
            0
        };

        Ok(RespValue::Integer(len as i64))
    }

    /// LRANGE key start stop
    /// Returns the specified elements of the list stored at key
    pub fn lrange(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LRANGE".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        let values = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let list = stored.as_list()?;
            let len = list.len() as i64;

            if len == 0 {
                Vec::new()
            } else {
                // Normalize negative indices
                let start_idx = if start < 0 {
                    (len + start).max(0) as usize
                } else {
                    start.min(len) as usize
                };

                let stop_idx = if stop < 0 {
                    (len + stop).max(0) as usize
                } else {
                    stop.min(len - 1) as usize
                };

                // Extract range
                if start_idx > stop_idx || start_idx >= len as usize {
                    Vec::new()
                } else {
                    list.iter()
                        .skip(start_idx)
                        .take(stop_idx - start_idx + 1)
                        .cloned()
                        .collect()
                }
            }
        } else {
            Vec::new()
        };

        Ok(RespValue::Array(Some(
            values.into_iter().map(RespValue::bulk_string).collect(),
        )))
    }

    /// LINDEX key index
    /// Returns the element at index in the list stored at key
    pub fn lindex(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 2 {
            return Err(AikvError::WrongArgCount("LINDEX".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let index = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid index".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        let value = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let list = stored.as_list()?;
            let len = list.len() as i64;

            if len == 0 {
                None
            } else {
                // Normalize negative index
                let idx = if index < 0 { len + index } else { index };

                if idx >= 0 && idx < len {
                    list.get(idx as usize).cloned()
                } else {
                    None
                }
            }
        } else {
            None
        };

        match value {
            Some(value) => Ok(RespValue::bulk_string(value)),
            None => Ok(RespValue::Null),
        }
    }

    /// LSET key index element
    /// Sets the list element at index to element
    pub fn lset(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LSET".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let index = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid index".to_string()))?;
        let element = args[2].clone();

        // Migrated: Logic moved from storage layer to command layer
        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut list = stored.as_list()?.clone();
            let len = list.len() as i64;

            // Normalize negative index
            let idx = if index < 0 { len + index } else { index };

            if idx >= 0 && idx < len {
                if let Some(elem) = list.get_mut(idx as usize) {
                    *elem = element;
                }
                self.storage
                    .set_value(db_index, key, StoredValue::new_list(list))?;
                Ok(RespValue::simple_string("OK"))
            } else {
                Err(AikvError::InvalidArgument("index out of range".to_string()))
            }
        } else {
            Err(AikvError::InvalidArgument("no such key".to_string()))
        }
    }

    /// LREM key count element
    /// Removes the first count occurrences of elements equal to element from the list
    pub fn lrem(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LREM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let count = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid count".to_string()))?;
        let element = args[2].clone();

        // Migrated: Logic moved from storage layer to command layer
        let removed = if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let mut list = stored.as_list()?.clone();
            let mut removed_count = 0;

            if count == 0 {
                // Remove all occurrences
                list.retain(|e| {
                    if e == &element {
                        removed_count += 1;
                        false
                    } else {
                        true
                    }
                });
            } else if count > 0 {
                // Remove first count occurrences from head
                let mut to_remove = count as usize;
                let mut new_list = VecDeque::new();
                for elem in list {
                    if to_remove > 0 && elem == element {
                        to_remove -= 1;
                        removed_count += 1;
                    } else {
                        new_list.push_back(elem);
                    }
                }
                list = new_list;
            } else {
                // Remove first |count| occurrences from tail
                let mut to_remove = (-count) as usize;
                let mut new_list = VecDeque::new();
                for elem in list.into_iter().rev() {
                    if to_remove > 0 && elem == element {
                        to_remove -= 1;
                        removed_count += 1;
                    } else {
                        new_list.push_front(elem);
                    }
                }
                list = new_list;
            }

            // Update or delete the list
            if list.is_empty() {
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                self.storage
                    .set_value(db_index, key, StoredValue::new_list(list))?;
            }

            removed_count
        } else {
            0
        };

        Ok(RespValue::Integer(removed as i64))
    }

    /// LTRIM key start stop
    /// Trim the list to the specified range
    pub fn ltrim(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 3 {
            return Err(AikvError::WrongArgCount("LTRIM".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let start = String::from_utf8_lossy(&args[1])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid start index".to_string()))?;
        let stop = String::from_utf8_lossy(&args[2])
            .parse::<i64>()
            .map_err(|_| AikvError::InvalidArgument("invalid stop index".to_string()))?;

        // Migrated: Logic moved from storage layer to command layer
        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let list = stored.as_list()?;
            let len = list.len() as i64;

            if len == 0 {
                // Empty list, just delete
                self.storage.delete_from_db(db_index, &key)?;
            } else {
                // Normalize negative indices
                let start_idx = if start < 0 {
                    (len + start).max(0) as usize
                } else {
                    start.min(len) as usize
                };

                let stop_idx = if stop < 0 {
                    (len + stop).max(0) as usize
                } else {
                    stop.min(len - 1) as usize
                };

                // Trim the list
                if start_idx > stop_idx || start_idx >= len as usize {
                    // Result would be empty
                    self.storage.delete_from_db(db_index, &key)?;
                } else {
                    let trimmed: VecDeque<Bytes> = list
                        .iter()
                        .skip(start_idx)
                        .take(stop_idx - start_idx + 1)
                        .cloned()
                        .collect();
                    self.storage
                        .set_value(db_index, key, StoredValue::new_list(trimmed))?;
                }
            }
        }

        Ok(RespValue::simple_string("OK"))
    }

    /// LINSERT key BEFORE|AFTER pivot element
    /// Inserts element in the list stored at key either before or after the reference value pivot
    pub fn linsert(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 4 {
            return Err(AikvError::WrongArgCount("LINSERT".to_string()));
        }

        let key = String::from_utf8_lossy(&args[0]).to_string();
        let position = String::from_utf8_lossy(&args[1]).to_uppercase();
        let pivot = args[2].clone();
        let element = args[3].clone();

        // Validate position argument
        let before = match position.as_str() {
            "BEFORE" => true,
            "AFTER" => false,
            _ => return Err(AikvError::InvalidArgument("ERR syntax error".to_string())),
        };

        if let Some(stored) = self.storage.get_value(db_index, &key)? {
            let list = stored.as_list()?.clone();

            // Find the pivot element
            let pivot_idx = list.iter().position(|e| e == &pivot);

            if let Some(idx) = pivot_idx {
                let insert_idx = if before { idx } else { idx + 1 };
                // VecDeque doesn't have insert, so we need to work around it
                let mut new_list: VecDeque<Bytes> = list.iter().take(insert_idx).cloned().collect();
                new_list.push_back(element);
                for elem in list.iter().skip(insert_idx) {
                    new_list.push_back(elem.clone());
                }

                let len = new_list.len();
                self.storage
                    .set_value(db_index, key, StoredValue::new_list(new_list))?;
                Ok(RespValue::Integer(len as i64))
            } else {
                // Pivot not found
                Ok(RespValue::Integer(-1))
            }
        } else {
            // Key doesn't exist
            Ok(RespValue::Integer(0))
        }
    }

    /// LMOVE source destination LEFT|RIGHT LEFT|RIGHT
    /// Atomically returns and removes the first/last element of the source list,
    /// and pushes the element to the first/last element of the destination list
    pub fn lmove(&self, args: &[Bytes], db_index: usize) -> Result<RespValue> {
        if args.len() != 4 {
            return Err(AikvError::WrongArgCount("LMOVE".to_string()));
        }

        let source_key = String::from_utf8_lossy(&args[0]).to_string();
        let dest_key = String::from_utf8_lossy(&args[1]).to_string();
        let wherefrom = String::from_utf8_lossy(&args[2]).to_uppercase();
        let whereto = String::from_utf8_lossy(&args[3]).to_uppercase();

        // Validate direction arguments
        let pop_left = match wherefrom.as_str() {
            "LEFT" => true,
            "RIGHT" => false,
            _ => return Err(AikvError::InvalidArgument("ERR syntax error".to_string())),
        };

        let push_left = match whereto.as_str() {
            "LEFT" => true,
            "RIGHT" => false,
            _ => return Err(AikvError::InvalidArgument("ERR syntax error".to_string())),
        };

        // Get source list
        if let Some(stored) = self.storage.get_value(db_index, &source_key)? {
            let mut source_list = stored.as_list()?.clone();

            if source_list.is_empty() {
                return Ok(RespValue::Null);
            }

            // Pop element from source
            let element = if pop_left {
                source_list.pop_front()
            } else {
                source_list.pop_back()
            };

            if let Some(elem) = element {
                // Get or create destination list
                let mut dest_list = if source_key == dest_key {
                    // Same key, use the already modified source list
                    source_list.clone()
                } else if let Some(dest_stored) = self.storage.get_value(db_index, &dest_key)? {
                    dest_stored.as_list()?.clone()
                } else {
                    VecDeque::new()
                };

                // Push element to destination
                if push_left {
                    dest_list.push_front(elem.clone());
                } else {
                    dest_list.push_back(elem.clone());
                }

                // Update source (if not same as dest)
                if source_key != dest_key {
                    if source_list.is_empty() {
                        self.storage.delete_from_db(db_index, &source_key)?;
                    } else {
                        self.storage.set_value(
                            db_index,
                            source_key,
                            StoredValue::new_list(source_list),
                        )?;
                    }
                }

                // Update destination
                self.storage
                    .set_value(db_index, dest_key, StoredValue::new_list(dest_list))?;

                Ok(RespValue::bulk_string(elem))
            } else {
                Ok(RespValue::Null)
            }
        } else {
            Ok(RespValue::Null)
        }
    }
}
