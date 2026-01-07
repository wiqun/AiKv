use bytes::Bytes;

/// RESP (REdis Serialization Protocol) value types
/// Supports both RESP2 and RESP3 protocol versions
#[derive(Debug, Clone, PartialEq)]
pub enum RespValue {
    // RESP2 types
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

    // RESP3 types
    /// Null: _\r\n (distinct from RESP2 null bulk string)
    Null,

    /// Boolean: #t\r\n or #f\r\n
    Boolean(bool),

    /// Double: ,3.14159\r\n or ,inf\r\n or ,-inf\r\n
    Double(f64),

    /// Big number: (3492890328409238509324850943850943825024385\r\n
    BigNumber(String),

    /// Bulk Error: !21\r\nSYNTAX invalid syntax\r\n
    BulkError(String),

    /// Verbatim String: =15\r\ntxt:Some string\r\n
    VerbatimString { format: String, data: Bytes },

    /// Map: %2\r\n+first\r\n:1\r\n+second\r\n:2\r\n
    Map(Vec<(RespValue, RespValue)>),

    /// Set: ~5\r\n+orange\r\n+apple\r\n...\r\n
    Set(Vec<RespValue>),

    /// Push: >4\r\n+pubsub\r\n+message\r\n+channel\r\n+message\r\n
    Push(Vec<RespValue>),

    /// Attribute: |1\r\n+key-popularity\r\n%2\r\n$1\r\na\r\n,0.1923\r\n$1\r\nb\r\n,0.0012\r\n
    /// Attributes are metadata attached to responses (RESP3)
    Attribute {
        attributes: Vec<(RespValue, RespValue)>,
        data: Box<RespValue>,
    },

    /// Streamed String: $?\r\n;4\r\nHell\r\n;5\r\no wor\r\n;1\r\nd\r\n;0\r\n
    /// For streaming large bulk strings in chunks (RESP3)
    StreamedString(Vec<Bytes>),
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

    // RESP3 helper methods

    /// Create a null response (RESP3)
    pub fn null() -> Self {
        RespValue::Null
    }

    /// Create a boolean response (RESP3)
    pub fn boolean(b: bool) -> Self {
        RespValue::Boolean(b)
    }

    /// Create a double response (RESP3)
    pub fn double(d: f64) -> Self {
        RespValue::Double(d)
    }

    /// Create a big number response (RESP3)
    pub fn big_number(s: impl Into<String>) -> Self {
        RespValue::BigNumber(s.into())
    }

    /// Create a bulk error response (RESP3)
    pub fn bulk_error(s: impl Into<String>) -> Self {
        RespValue::BulkError(s.into())
    }

    /// Create a verbatim string response (RESP3)
    pub fn verbatim_string(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
        RespValue::VerbatimString {
            format: format.into(),
            data: data.into(),
        }
    }

    /// Create a map response (RESP3)
    pub fn map(pairs: Vec<(RespValue, RespValue)>) -> Self {
        RespValue::Map(pairs)
    }

    /// Create a set response (RESP3)
    pub fn set(items: Vec<RespValue>) -> Self {
        RespValue::Set(items)
    }

    /// Create a push response (RESP3)
    pub fn push(items: Vec<RespValue>) -> Self {
        RespValue::Push(items)
    }

    /// Create an attribute response (RESP3)
    pub fn attribute(attributes: Vec<(RespValue, RespValue)>, data: RespValue) -> Self {
        RespValue::Attribute {
            attributes,
            data: Box::new(data),
        }
    }

    /// Create a streamed string response (RESP3)
    pub fn streamed_string(chunks: Vec<Bytes>) -> Self {
        RespValue::StreamedString(chunks)
    }

    /// Serialize to RESP format bytes
    /// Supports both RESP2 and RESP3 formats
    pub fn serialize(&self) -> Bytes {
        match self {
            // RESP2 types
            RespValue::SimpleString(s) => Bytes::from(format!("+{}\r\n", s)),
            RespValue::Error(e) => Bytes::from(format!("-{}\r\n", e)),
            RespValue::Integer(i) => Bytes::from(format!(":{}\r\n", i)),
            RespValue::BulkString(None) => Bytes::from("$-1\r\n"),
            RespValue::BulkString(Some(s)) => {
                // Build binary-safe bulk string: $<len>\r\n<data>\r\n
                let header = format!("${}\r\n", s.len());
                let mut result = Vec::with_capacity(header.len() + s.len() + 2);
                result.extend_from_slice(header.as_bytes());
                result.extend_from_slice(s);
                result.extend_from_slice(b"\r\n");
                Bytes::from(result)
            }
            RespValue::Array(None) => Bytes::from("*-1\r\n"),
            RespValue::Array(Some(arr)) => {
                let mut result = format!("*{}\r\n", arr.len()).into_bytes();
                for item in arr {
                    result.extend_from_slice(&item.serialize());
                }
                Bytes::from(result)
            }
            // RESP3 types
            RespValue::Null => Bytes::from("_\r\n"),
            RespValue::Boolean(b) => {
                if *b {
                    Bytes::from("#t\r\n")
                } else {
                    Bytes::from("#f\r\n")
                }
            }
            RespValue::Double(d) => {
                if d.is_infinite() {
                    if d.is_sign_positive() {
                        Bytes::from(",inf\r\n")
                    } else {
                        Bytes::from(",-inf\r\n")
                    }
                } else {
                    Bytes::from(format!(",{}\r\n", d))
                }
            }
            RespValue::BigNumber(s) => Bytes::from(format!("({}\r\n", s)),
            RespValue::BulkError(e) => {
                let bytes = e.as_bytes();
                Bytes::from(format!("!{}\r\n{}\r\n", bytes.len(), e))
            }
            RespValue::VerbatimString {
                format,
                data,
            } => {
                let total_len = format.len() + 1 + data.len(); // format + ':' + data
                let header = format!("={}\r\n{}:", total_len, format);
                let mut result = Vec::with_capacity(header.len() + data.len() + 2);
                result.extend_from_slice(header.as_bytes());
                result.extend_from_slice(data);
                result.extend_from_slice(b"\r\n");
                Bytes::from(result)
            }
            RespValue::Map(pairs) => {
                let mut result = format!("%{}\r\n", pairs.len()).into_bytes();
                for (key, value) in pairs {
                    result.extend_from_slice(&key.serialize());
                    result.extend_from_slice(&value.serialize());
                }
                Bytes::from(result)
            }
            RespValue::Set(items) => {
                let mut result = format!("~{}\r\n", items.len()).into_bytes();
                for item in items {
                    result.extend_from_slice(&item.serialize());
                }
                Bytes::from(result)
            }
            RespValue::Push(items) => {
                let mut result = format!(">{}\r\n", items.len()).into_bytes();
                for item in items {
                    result.extend_from_slice(&item.serialize());
                }
                Bytes::from(result)
            }
            RespValue::Attribute {
                attributes,
                data,
            } => {
                // Serialize attributes map followed by the actual data
                let mut result = format!("|{}\r\n", attributes.len()).into_bytes();
                for (key, value) in attributes {
                    result.extend_from_slice(&key.serialize());
                    result.extend_from_slice(&value.serialize());
                }
                // Append the actual data
                result.extend_from_slice(&data.serialize());
                Bytes::from(result)
            }
            RespValue::StreamedString(chunks) => {
                // Streamed string format: $?\r\n;len\r\ndata\r\n...;0\r\n
                let mut result = Vec::from("$?\r\n".as_bytes());
                for chunk in chunks {
                    result.extend_from_slice(format!(";{}\r\n", chunk.len()).as_bytes());
                    result.extend_from_slice(chunk);
                    result.extend_from_slice(b"\r\n");
                }
                // Terminator
                result.extend_from_slice(b";0\r\n");
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

    // RESP3 tests
    #[test]
    fn test_null() {
        let val = RespValue::null();
        assert_eq!(val.serialize(), Bytes::from("_\r\n"));
    }

    #[test]
    fn test_boolean_true() {
        let val = RespValue::boolean(true);
        assert_eq!(val.serialize(), Bytes::from("#t\r\n"));
    }

    #[test]
    fn test_boolean_false() {
        let val = RespValue::boolean(false);
        assert_eq!(val.serialize(), Bytes::from("#f\r\n"));
    }

    #[test]
    fn test_double() {
        let val = RespValue::double(1.23456);
        assert_eq!(val.serialize(), Bytes::from(",1.23456\r\n"));
    }

    #[test]
    fn test_double_infinity() {
        let val = RespValue::double(f64::INFINITY);
        assert_eq!(val.serialize(), Bytes::from(",inf\r\n"));
    }

    #[test]
    fn test_double_negative_infinity() {
        let val = RespValue::double(f64::NEG_INFINITY);
        assert_eq!(val.serialize(), Bytes::from(",-inf\r\n"));
    }

    #[test]
    fn test_big_number() {
        let val = RespValue::big_number("3492890328409238509324850943850943825024385");
        assert_eq!(
            val.serialize(),
            Bytes::from("(3492890328409238509324850943850943825024385\r\n")
        );
    }

    #[test]
    fn test_bulk_error() {
        let val = RespValue::bulk_error("SYNTAX invalid syntax");
        assert_eq!(
            val.serialize(),
            Bytes::from("!21\r\nSYNTAX invalid syntax\r\n")
        );
    }

    #[test]
    fn test_verbatim_string() {
        let val = RespValue::verbatim_string("txt", "Some string");
        assert_eq!(val.serialize(), Bytes::from("=15\r\ntxt:Some string\r\n"));
    }

    #[test]
    fn test_map() {
        let val = RespValue::map(vec![
            (RespValue::simple_string("first"), RespValue::integer(1)),
            (RespValue::simple_string("second"), RespValue::integer(2)),
        ]);
        assert_eq!(
            val.serialize(),
            Bytes::from("%2\r\n+first\r\n:1\r\n+second\r\n:2\r\n")
        );
    }

    #[test]
    fn test_set() {
        let val = RespValue::set(vec![
            RespValue::simple_string("orange"),
            RespValue::simple_string("apple"),
        ]);
        assert_eq!(val.serialize(), Bytes::from("~2\r\n+orange\r\n+apple\r\n"));
    }

    #[test]
    fn test_push() {
        let val = RespValue::push(vec![
            RespValue::simple_string("pubsub"),
            RespValue::simple_string("message"),
            RespValue::simple_string("channel"),
            RespValue::bulk_string("Hello"),
        ]);
        assert_eq!(
            val.serialize(),
            Bytes::from(">4\r\n+pubsub\r\n+message\r\n+channel\r\n$5\r\nHello\r\n")
        );
    }

    #[test]
    fn test_attribute() {
        let val = RespValue::attribute(
            vec![(RespValue::simple_string("ttl"), RespValue::integer(3600))],
            RespValue::simple_string("OK"),
        );
        assert_eq!(
            val.serialize(),
            Bytes::from("|1\r\n+ttl\r\n:3600\r\n+OK\r\n")
        );
    }

    #[test]
    fn test_streamed_string() {
        let val = RespValue::streamed_string(vec![
            Bytes::from("Hell"),
            Bytes::from("o wor"),
            Bytes::from("ld"),
        ]);
        assert_eq!(
            val.serialize(),
            Bytes::from("$?\r\n;4\r\nHell\r\n;5\r\no wor\r\n;2\r\nld\r\n;0\r\n")
        );
    }

    #[test]
    fn test_attribute_with_complex_data() {
        let val = RespValue::attribute(
            vec![
                (
                    RespValue::simple_string("server"),
                    RespValue::simple_string("aikv"),
                ),
                (RespValue::simple_string("version"), RespValue::double(1.0)),
            ],
            RespValue::array(vec![
                RespValue::bulk_string("value1"),
                RespValue::bulk_string("value2"),
            ]),
        );
        assert_eq!(
            val.serialize(),
            Bytes::from("|2\r\n+server\r\n+aikv\r\n+version\r\n,1\r\n*2\r\n$6\r\nvalue1\r\n$6\r\nvalue2\r\n")
        );
    }

    #[test]
    fn test_bulk_string_binary_data() {
        // Test with binary data that is NOT valid UTF-8
        // This would previously cause "Invalid bulk string terminator" errors
        // because String::from_utf8_lossy would corrupt the data
        let binary_data: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01, 0x80, 0x90];
        let val = RespValue::bulk_string(Bytes::from(binary_data.clone()));
        let serialized = val.serialize();

        // Should be: $6\r\n<6 bytes of binary data>\r\n
        let expected_len = 4 + binary_data.len() + 2; // "$6\r\n" + data + "\r\n"
        assert_eq!(serialized.len(), expected_len);

        // Verify the structure
        assert_eq!(&serialized[0..4], b"$6\r\n");
        assert_eq!(&serialized[4..10], binary_data.as_slice());
        assert_eq!(&serialized[10..12], b"\r\n");
    }

    #[test]
    fn test_array_with_binary_bulk_strings() {
        // Test nested arrays with binary data
        let binary1: Vec<u8> = vec![0xFF, 0x00];
        let binary2: Vec<u8> = vec![0x80, 0x81, 0x82];

        let val = RespValue::array(vec![
            RespValue::bulk_string(Bytes::from(binary1)),
            RespValue::bulk_string(Bytes::from(binary2)),
        ]);

        let serialized = val.serialize();

        // Should be: *2\r\n$2\r\n<2 bytes>\r\n$3\r\n<3 bytes>\r\n
        // Total: 4 + 4 + 2 + 2 + 4 + 3 + 2 = 21 bytes
        assert_eq!(serialized.len(), 21);

        // Verify structure
        assert_eq!(&serialized[0..4], b"*2\r\n");
        assert_eq!(&serialized[4..8], b"$2\r\n");
        assert_eq!(&serialized[8..10], &[0xFF, 0x00]);
        assert_eq!(&serialized[10..12], b"\r\n");
        assert_eq!(&serialized[12..16], b"$3\r\n");
        assert_eq!(&serialized[16..19], &[0x80, 0x81, 0x82]);
        assert_eq!(&serialized[19..21], b"\r\n");
    }
}
