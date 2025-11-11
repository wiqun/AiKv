use bytes::Bytes;

/// RESP (REdis Serialization Protocol) value types
#[derive(Debug, Clone, PartialEq)]
pub enum RespValue {
    /// Simple String: +OK\r\n
    SimpleString(String),
    
    /// Error: -Error message\r\n
    Error(String),
    
    /// Integer: :1000\r\n
    Integer(i64),
    
    /// Bulk String: $6\r\nfoobar\r\n or $-1\r\n for null
    BulkString(Option<Bytes>),
    
    /// Array: *2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n or *-1\r\n for null
    Array(Option<Vec<RespValue>>),
}

impl RespValue {
    /// Create a simple string response
    pub fn simple_string(s: impl Into<String>) -> Self {
        RespValue::SimpleString(s.into())
    }

    /// Create an error response
    pub fn error(s: impl Into<String>) -> Self {
        RespValue::Error(s.into())
    }

    /// Create an integer response
    pub fn integer(i: i64) -> Self {
        RespValue::Integer(i)
    }

    /// Create a bulk string response
    pub fn bulk_string(s: impl Into<Bytes>) -> Self {
        RespValue::BulkString(Some(s.into()))
    }

    /// Create a null bulk string response
    pub fn null_bulk_string() -> Self {
        RespValue::BulkString(None)
    }

    /// Create an array response
    pub fn array(arr: Vec<RespValue>) -> Self {
        RespValue::Array(Some(arr))
    }

    /// Create a null array response
    pub fn null_array() -> Self {
        RespValue::Array(None)
    }

    /// Create OK response
    pub fn ok() -> Self {
        RespValue::SimpleString("OK".to_string())
    }

    /// Serialize to RESP format bytes
    pub fn serialize(&self) -> Bytes {
        match self {
            RespValue::SimpleString(s) => {
                Bytes::from(format!("+{}\r\n", s))
            }
            RespValue::Error(e) => {
                Bytes::from(format!("-{}\r\n", e))
            }
            RespValue::Integer(i) => {
                Bytes::from(format!(":{}\r\n", i))
            }
            RespValue::BulkString(None) => {
                Bytes::from("$-1\r\n")
            }
            RespValue::BulkString(Some(s)) => {
                let mut result = format!("${}\r\n", s.len());
                result.push_str(&String::from_utf8_lossy(s));
                result.push_str("\r\n");
                Bytes::from(result)
            }
            RespValue::Array(None) => {
                Bytes::from("*-1\r\n")
            }
            RespValue::Array(Some(arr)) => {
                let mut result = format!("*{}\r\n", arr.len());
                for item in arr {
                    result.push_str(&String::from_utf8_lossy(&item.serialize()));
                }
                Bytes::from(result)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_string() {
        let val = RespValue::simple_string("OK");
        assert_eq!(val.serialize(), Bytes::from("+OK\r\n"));
    }

    #[test]
    fn test_error() {
        let val = RespValue::error("Error message");
        assert_eq!(val.serialize(), Bytes::from("-Error message\r\n"));
    }

    #[test]
    fn test_integer() {
        let val = RespValue::integer(1000);
        assert_eq!(val.serialize(), Bytes::from(":1000\r\n"));
    }

    #[test]
    fn test_bulk_string() {
        let val = RespValue::bulk_string("foobar");
        assert_eq!(val.serialize(), Bytes::from("$6\r\nfoobar\r\n"));
    }

    #[test]
    fn test_null_bulk_string() {
        let val = RespValue::null_bulk_string();
        assert_eq!(val.serialize(), Bytes::from("$-1\r\n"));
    }

    #[test]
    fn test_array() {
        let val = RespValue::array(vec![
            RespValue::bulk_string("foo"),
            RespValue::bulk_string("bar"),
        ]);
        assert_eq!(
            val.serialize(),
            Bytes::from("*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n")
        );
    }

    #[test]
    fn test_null_array() {
        let val = RespValue::null_array();
        assert_eq!(val.serialize(), Bytes::from("*-1\r\n"));
    }
}
