use serde_json::{json, Value as JsonValue};

use crate::error::Result;

use super::{err, JsonPathEngine};

/// 从 JSON 节点按路径提取值,支持点号分隔的字段路径和数组索引.
pub(super) fn traverse_path(item: &JsonValue, field: &str) -> JsonValue {
    if field.is_empty() {
        return item.clone();
    }
    let parts = JsonPathEngine::split_path_parts(field);
    let mut cur = item;
    let mut found = JsonValue::Null;
    let len = parts.len();
    for (i, part) in parts.iter().enumerate() {
        match cur {
            JsonValue::Object(obj) => {
                if i == len - 1 {
                    found = obj.get(part.as_str()).cloned().unwrap_or_default();
                } else if part.starts_with('[') && part.ends_with(']') {
                    let inner = &part[1..part.len() - 1];
                    if let Ok(idx) = inner.parse::<usize>() {
                        if let JsonValue::Array(arr) = cur {
                            cur = arr.get(idx).unwrap_or(&JsonValue::Null);
                            continue;
                        }
                    }
                    found = JsonValue::Null;
                    break;
                } else {
                    cur = obj.get(part.as_str()).unwrap_or(&JsonValue::Null);
                }
            }
            JsonValue::Array(arr) if part.starts_with('[') && part.ends_with(']') => {
                let inner = &part[1..part.len() - 1];
                if let Ok(idx) = inner.parse::<usize>() {
                    if i == len - 1 {
                        found = arr.get(idx).cloned().unwrap_or(JsonValue::Null);
                    } else {
                        cur = arr.get(idx).unwrap_or(&JsonValue::Null);
                    }
                } else {
                    found = JsonValue::Null;
                    break;
                }
            }
            JsonValue::Array(_) => {
                found = JsonValue::Null;
                break;
            }
            _ => {
                found = JsonValue::Null;
                break;
            }
        }
    }
    found
}

impl JsonPathEngine {
    pub fn extract(&self, json: &JsonValue, path: &str) -> Result<JsonValue> {
        // Remove leading $ or .
        let path = path.trim_start_matches('$').trim_start_matches('.');

        if path.is_empty() {
            return Ok(json.clone());
        }

        // Handle root array wildcard $[*]
        if path == "[*]" {
            if let JsonValue::Array(arr) = json {
                // Return array of all elements
                return Ok(JsonValue::Array(arr.clone()));
            }
            return Ok(json.clone());
        }

        // Split by '.' but preserve bracket syntax
        let parts = Self::split_path_parts(path);
        let mut current = json.clone();

        for part in &parts {
            if part == "*" {
                // Wildcard - return current as-is if array, or wrap in array
                if let JsonValue::Array(_) = current {
                    // Already an array, keep as is
                } else {
                    // Wrap single value in array
                    current = json!(vec![current]);
                }
                continue;
            }

            if part.starts_with('[') && part.ends_with(']') {
                // Array index or field selector like [0] or ['field1','field2']
                let inner = &part[1..part.len() - 1];

                if inner.starts_with('\'') {
                    // Field selector: ['field1','field2'] - flatten values in order
                    // Flatten values into [val1, val2, ...] instead of [{field1:val1, field2:val2}, ...]
                    let inner_str: &str = inner;
                    let fields: Vec<&str> = inner_str
                        .trim_start_matches('\'')
                        .trim_end_matches('\'')
                        .split("','")
                        .collect();

                    if let JsonValue::Array(arr) = &current {
                        // Array of objects - flatten all values in order
                        let mut result_array = Vec::new();
                        for item in arr {
                            for field in &fields {
                                if let JsonValue::Object(obj) = item {
                                    let value = obj
                                        .get(*field)
                                        .cloned()
                                        .unwrap_or(JsonValue::String(String::new()));
                                    result_array.push(value);
                                }
                            }
                        }
                        current = JsonValue::Array(result_array);
                    } else if let JsonValue::Object(obj) = &current {
                        // Single object - flatten values in order
                        let mut result_array = Vec::new();
                        for field in &fields {
                            let value = obj
                                .get(*field)
                                .cloned()
                                .unwrap_or(JsonValue::String(String::new()));
                            result_array.push(value);
                        }
                        current = JsonValue::Array(result_array);
                    } else {
                        return Err(err(format!("Cannot traverse non-object at: {}", part)));
                    }
                } else if inner.starts_with("?(") {
                    // JSONPath filter expression: [?(@.field == value)] or [?(@ > value)]
                    if let JsonValue::Array(arr) = &current {
                        // Flatten nested arrays: when a field access on an array of
                        // objects produces [[val1, val2, ...]], the filter should
                        // target the inner array elements, not the outer wrapper.
                        let target = if arr.len() == 1 {
                            if let Some(JsonValue::Array(inner_arr)) = arr.first() {
                                inner_arr
                            } else {
                                arr
                            }
                        } else {
                            arr
                        };
                        let filtered = self.filter_jsonpath(target, inner)?;
                        current = JsonValue::Array(filtered);
                    } else {
                        return Err(err(format!("Cannot apply filter on non-array: {}", part)));
                    }
                } else if inner == "*" {
                    // Array wildcard [*] - return all elements
                    if let JsonValue::Array(arr) = &current {
                        current = JsonValue::Array(arr.clone());
                    } else {
                        // If not array, return as single element array
                        current = json!(vec![current]);
                    }
                } else if inner.starts_with('-') || inner.parse::<i64>().is_ok_and(|n| n < 0) {
                    return Err(err(format!("Array index must be non-negative: [{inner}]")));
                } else if let Ok(idx) = inner.parse::<usize>() {
                    if let JsonValue::Array(arr) = &current {
                        if idx >= arr.len() {
                            return Err(err(format!("Array index out of bounds: {idx}")));
                        }
                        current = arr[idx].clone();
                    } else {
                        return Err(err(format!("Cannot index non-array: {part}")));
                    }
                }
            } else {
                // Regular object key access
                if let JsonValue::Object(obj) = &current {
                    current = obj
                        .get(part.as_str())
                        .cloned()
                        .ok_or_else(|| err(format!("Path not found: {}", part)))?;
                } else if let JsonValue::Array(arr) = &current {
                    // Trying to access field on array - apply to each element
                    let mut result_array = Vec::new();
                    for item in arr {
                        if let JsonValue::Object(obj) = item {
                            if let Some(value) = obj.get(part.as_str()) {
                                result_array.push(value.clone());
                            }
                        }
                    }
                    current = JsonValue::Array(result_array);
                } else {
                    return Err(err(format!("Cannot traverse non-object at: {}", part)));
                }
            }
        }

        Ok(current)
    }
}
