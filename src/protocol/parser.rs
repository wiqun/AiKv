use super::types::RespValue;
use crate::error::{AikvError, Result};
use bytes::{Buf, Bytes, BytesMut};

/// RESP protocol parser
pub struct RespParser {
    buffer: BytesMut,
}

impl RespParser {
    /// Create a new parser with a given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: BytesMut::with_capacity(capacity),
        }
    }

    /// Add data to the parser buffer
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Get a mutable reference to the buffer
    pub fn buffer_mut(&mut self) -> &mut BytesMut {
        &mut self.buffer
    }

    /// Try to parse a complete RESP value from the buffer
    pub fn parse(&mut self) -> Result<Option<RespValue>> {
        if self.buffer.is_empty() {
            return Ok(None);
        }

        let mut cursor = std::io::Cursor::new(&self.buffer[..]);
        match self.parse_value(&mut cursor) {
            Ok(value) => {
                let pos = cursor.position() as usize;
                self.buffer.advance(pos);
                Ok(Some(value))
            }
            Err(AikvError::Protocol(_)) => Ok(None), // Need more data
            Err(e) => Err(e),
        }
    }

    fn parse_value(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        if cursor.position() >= cursor.get_ref().len() as u64 {
            return Err(AikvError::Protocol("Incomplete data".to_string()));
        }

        let byte = cursor.get_ref()[cursor.position() as usize];
        cursor.set_position(cursor.position() + 1);

        match byte {
            // RESP2 types
            b'+' => self.parse_simple_string(cursor),
            b'-' => self.parse_error(cursor),
            b':' => self.parse_integer(cursor),
            b'$' => self.parse_bulk_string(cursor),
            b'*' => self.parse_array(cursor),
            // RESP3 types
            b'_' => self.parse_null(cursor),
            b'#' => self.parse_boolean(cursor),
            b',' => self.parse_double(cursor),
            b'(' => self.parse_big_number(cursor),
            b'!' => self.parse_bulk_error(cursor),
            b'=' => self.parse_verbatim_string(cursor),
            b'%' => self.parse_map(cursor),
            b'~' => self.parse_set(cursor),
            b'>' => self.parse_push(cursor),
            b'|' => self.parse_attribute(cursor),
            b';' => self.parse_streamed_chunk(cursor),
            _ => Err(AikvError::Protocol(format!(
                "Invalid RESP type marker: {}",
                byte as char
            ))),
        }
    }

    fn parse_simple_string(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        Ok(RespValue::SimpleString(line))
    }

    fn parse_error(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        Ok(RespValue::Error(line))
    }

    fn parse_integer(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let num = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid integer: {}", line)))?;
        Ok(RespValue::Integer(num))
    }

    fn parse_bulk_string(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;

        // Check for streamed string marker
        if line == "?" {
            return self.parse_streamed_string_body(cursor);
        }

        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid bulk string length: {}", line)))?;

        if len == -1 {
            return Ok(RespValue::BulkString(None));
        }

        if len < 0 {
            return Err(AikvError::Protocol(format!(
                "Invalid bulk string length: {}",
                len
            )));
        }

        let len = len as usize;
        let pos = cursor.position() as usize;
        let data = cursor.get_ref();

        if pos + len + 2 > data.len() {
            return Err(AikvError::Protocol("Incomplete bulk string".to_string()));
        }

        let bytes = Bytes::copy_from_slice(&data[pos..pos + len]);
        cursor.set_position((pos + len + 2) as u64); // Skip \r\n

        Ok(RespValue::BulkString(Some(bytes)))
    }

    fn parse_array(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid array length: {}", line)))?;

        if len == -1 {
            return Ok(RespValue::Array(None));
        }

        if len < 0 {
            return Err(AikvError::Protocol(format!(
                "Invalid array length: {}",
                len
            )));
        }

        let mut array = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let value = self.parse_value(cursor)?;
            array.push(value);
        }

        Ok(RespValue::Array(Some(array)))
    }

    fn read_line(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<String> {
        let start = cursor.position() as usize;
        let data = cursor.get_ref();

        for i in start..data.len() - 1 {
            if data[i] == b'\r' && data[i + 1] == b'\n' {
                let line = String::from_utf8_lossy(&data[start..i]).to_string();
                cursor.set_position((i + 2) as u64);
                return Ok(line);
            }
        }

        Err(AikvError::Protocol("Incomplete line".to_string()))
    }

    // RESP3 parsing methods

    fn parse_null(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let _ = self.read_line(cursor)?; // Read the \r\n
        Ok(RespValue::Null)
    }

    fn parse_boolean(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        match line.as_str() {
            "t" => Ok(RespValue::Boolean(true)),
            "f" => Ok(RespValue::Boolean(false)),
            _ => Err(AikvError::Protocol(format!("Invalid boolean: {}", line))),
        }
    }

    fn parse_double(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let num = match line.as_str() {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            _ => line
                .parse::<f64>()
                .map_err(|_| AikvError::Protocol(format!("Invalid double: {}", line)))?,
        };
        Ok(RespValue::Double(num))
    }

    fn parse_big_number(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        Ok(RespValue::BigNumber(line))
    }

    fn parse_bulk_error(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid bulk error length: {}", line)))?;

        if len < 0 {
            return Err(AikvError::Protocol(format!(
                "Invalid bulk error length: {}",
                len
            )));
        }

        let len = len as usize;
        let pos = cursor.position() as usize;
        let data = cursor.get_ref();

        if pos + len + 2 > data.len() {
            return Err(AikvError::Protocol("Incomplete bulk error".to_string()));
        }

        let error_str = String::from_utf8_lossy(&data[pos..pos + len]).to_string();
        cursor.set_position((pos + len + 2) as u64); // Skip \r\n

        Ok(RespValue::BulkError(error_str))
    }

    fn parse_verbatim_string(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line.parse::<i64>().map_err(|_| {
            AikvError::Protocol(format!("Invalid verbatim string length: {}", line))
        })?;

        if len < 0 {
            return Err(AikvError::Protocol(format!(
                "Invalid verbatim string length: {}",
                len
            )));
        }

        let len = len as usize;
        let pos = cursor.position() as usize;
        let data = cursor.get_ref();

        if pos + len + 2 > data.len() {
            return Err(AikvError::Protocol(
                "Incomplete verbatim string".to_string(),
            ));
        }

        // Parse format:data structure
        let content = &data[pos..pos + len];
        let colon_pos = content
            .iter()
            .position(|&b| b == b':')
            .ok_or_else(|| AikvError::Protocol("Invalid verbatim string format".to_string()))?;

        let format = String::from_utf8_lossy(&content[..colon_pos]).to_string();
        let data_bytes = Bytes::copy_from_slice(&content[colon_pos + 1..]);

        cursor.set_position((pos + len + 2) as u64); // Skip \r\n

        Ok(RespValue::VerbatimString {
            format,
            data: data_bytes,
        })
    }

    fn parse_map(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid map length: {}", line)))?;

        if len < 0 {
            return Err(AikvError::Protocol(format!("Invalid map length: {}", len)));
        }

        let mut pairs = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let key = self.parse_value(cursor)?;
            let value = self.parse_value(cursor)?;
            pairs.push((key, value));
        }

        Ok(RespValue::Map(pairs))
    }

    fn parse_set(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid set length: {}", line)))?;

        if len < 0 {
            return Err(AikvError::Protocol(format!("Invalid set length: {}", len)));
        }

        let mut items = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let value = self.parse_value(cursor)?;
            items.push(value);
        }

        Ok(RespValue::Set(items))
    }

    fn parse_push(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid push length: {}", line)))?;

        if len < 0 {
            return Err(AikvError::Protocol(format!("Invalid push length: {}", len)));
        }

        let mut items = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let value = self.parse_value(cursor)?;
            items.push(value);
        }

        Ok(RespValue::Push(items))
    }

    fn parse_attribute(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let line = self.read_line(cursor)?;
        let len = line
            .parse::<i64>()
            .map_err(|_| AikvError::Protocol(format!("Invalid attribute length: {}", line)))?;

        if len < 0 {
            return Err(AikvError::Protocol(format!(
                "Invalid attribute length: {}",
                len
            )));
        }

        let mut attributes = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let key = self.parse_value(cursor)?;
            let value = self.parse_value(cursor)?;
            attributes.push((key, value));
        }

        // After attributes, parse the actual data
        let data = self.parse_value(cursor)?;

        Ok(RespValue::Attribute {
            attributes,
            data: Box::new(data),
        })
    }

    fn parse_streamed_string_body(&self, cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        let mut chunks = Vec::new();

        loop {
            // Expect ';' marker for each chunk
            if cursor.position() >= cursor.get_ref().len() as u64 {
                return Err(AikvError::Protocol(
                    "Incomplete streamed string".to_string(),
                ));
            }

            let byte = cursor.get_ref()[cursor.position() as usize];
            if byte != b';' {
                return Err(AikvError::Protocol(format!(
                    "Expected ';' in streamed string, got {}",
                    byte as char
                )));
            }
            cursor.set_position(cursor.position() + 1);

            let line = self.read_line(cursor)?;
            let len = line.parse::<usize>().map_err(|_| {
                AikvError::Protocol(format!("Invalid streamed chunk length: {}", line))
            })?;

            // Length 0 means end of stream
            if len == 0 {
                break;
            }

            let pos = cursor.position() as usize;
            let data = cursor.get_ref();

            if pos + len + 2 > data.len() {
                return Err(AikvError::Protocol("Incomplete streamed chunk".to_string()));
            }

            let chunk = Bytes::copy_from_slice(&data[pos..pos + len]);
            cursor.set_position((pos + len + 2) as u64); // Skip \r\n
            chunks.push(chunk);
        }

        Ok(RespValue::StreamedString(chunks))
    }

    fn parse_streamed_chunk(&self, _cursor: &mut std::io::Cursor<&[u8]>) -> Result<RespValue> {
        // This should not be called directly as ';' is handled within streamed string parsing
        Err(AikvError::Protocol(
            "Unexpected ';' marker outside streamed string context".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_string() {
        let mut parser = RespParser::new(128);
        parser.feed(b"+OK\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::SimpleString("OK".to_string())));
    }

    #[test]
    fn test_parse_error() {
        let mut parser = RespParser::new(128);
        parser.feed(b"-Error message\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Error("Error message".to_string())));
    }

    #[test]
    fn test_parse_integer() {
        let mut parser = RespParser::new(128);
        parser.feed(b":1000\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Integer(1000)));
    }

    #[test]
    fn test_parse_bulk_string() {
        let mut parser = RespParser::new(128);
        parser.feed(b"$6\r\nfoobar\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::BulkString(Some(Bytes::from("foobar"))))
        );
    }

    #[test]
    fn test_parse_null_bulk_string() {
        let mut parser = RespParser::new(128);
        parser.feed(b"$-1\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::BulkString(None)));
    }

    #[test]
    fn test_parse_array() {
        let mut parser = RespParser::new(128);
        parser.feed(b"*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Array(Some(vec![
                RespValue::BulkString(Some(Bytes::from("foo"))),
                RespValue::BulkString(Some(Bytes::from("bar"))),
            ])))
        );
    }

    #[test]
    fn test_parse_incomplete_data() {
        let mut parser = RespParser::new(128);
        parser.feed(b"+OK");

        let result = parser.parse().unwrap();
        assert_eq!(result, None);
    }

    // RESP3 tests
    #[test]
    fn test_parse_null() {
        let mut parser = RespParser::new(128);
        parser.feed(b"_\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Null));
    }

    #[test]
    fn test_parse_boolean_true() {
        let mut parser = RespParser::new(128);
        parser.feed(b"#t\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Boolean(true)));
    }

    #[test]
    fn test_parse_boolean_false() {
        let mut parser = RespParser::new(128);
        parser.feed(b"#f\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Boolean(false)));
    }

    #[test]
    fn test_parse_double() {
        let mut parser = RespParser::new(128);
        parser.feed(b",1.23456\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Double(1.23456)));
    }

    #[test]
    fn test_parse_double_infinity() {
        let mut parser = RespParser::new(128);
        parser.feed(b",inf\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(result, Some(RespValue::Double(f64::INFINITY)));
    }

    #[test]
    fn test_parse_big_number() {
        let mut parser = RespParser::new(128);
        parser.feed(b"(3492890328409238509324850943850943825024385\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::BigNumber(
                "3492890328409238509324850943850943825024385".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_bulk_error() {
        let mut parser = RespParser::new(128);
        parser.feed(b"!21\r\nSYNTAX invalid syntax\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::BulkError("SYNTAX invalid syntax".to_string()))
        );
    }

    #[test]
    fn test_parse_verbatim_string() {
        let mut parser = RespParser::new(128);
        parser.feed(b"=15\r\ntxt:Some string\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::VerbatimString {
                format: "txt".to_string(),
                data: Bytes::from("Some string")
            })
        );
    }

    #[test]
    fn test_parse_map() {
        let mut parser = RespParser::new(128);
        parser.feed(b"%2\r\n+first\r\n:1\r\n+second\r\n:2\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Map(vec![
                (
                    RespValue::SimpleString("first".to_string()),
                    RespValue::Integer(1)
                ),
                (
                    RespValue::SimpleString("second".to_string()),
                    RespValue::Integer(2)
                ),
            ]))
        );
    }

    #[test]
    fn test_parse_set() {
        let mut parser = RespParser::new(128);
        parser.feed(b"~2\r\n+orange\r\n+apple\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Set(vec![
                RespValue::SimpleString("orange".to_string()),
                RespValue::SimpleString("apple".to_string()),
            ]))
        );
    }

    #[test]
    fn test_parse_push() {
        let mut parser = RespParser::new(128);
        parser.feed(b">3\r\n+pubsub\r\n+message\r\n+Hello\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Push(vec![
                RespValue::SimpleString("pubsub".to_string()),
                RespValue::SimpleString("message".to_string()),
                RespValue::SimpleString("Hello".to_string()),
            ]))
        );
    }

    #[test]
    fn test_parse_attribute() {
        let mut parser = RespParser::new(256);
        parser.feed(b"|1\r\n+ttl\r\n:3600\r\n+OK\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Attribute {
                attributes: vec![(
                    RespValue::SimpleString("ttl".to_string()),
                    RespValue::Integer(3600)
                )],
                data: Box::new(RespValue::SimpleString("OK".to_string()))
            })
        );
    }

    #[test]
    fn test_parse_streamed_string() {
        let mut parser = RespParser::new(256);
        parser.feed(b"$?\r\n;4\r\nHell\r\n;2\r\no!\r\n;0\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::StreamedString(vec![
                Bytes::from("Hell"),
                Bytes::from("o!"),
            ]))
        );
    }

    #[test]
    fn test_parse_attribute_with_array() {
        let mut parser = RespParser::new(512);
        parser
            .feed(b"|2\r\n+key1\r\n+val1\r\n+key2\r\n:42\r\n*2\r\n$5\r\nhello\r\n$5\r\nworld\r\n");

        let result = parser.parse().unwrap();
        assert_eq!(
            result,
            Some(RespValue::Attribute {
                attributes: vec![
                    (
                        RespValue::SimpleString("key1".to_string()),
                        RespValue::SimpleString("val1".to_string())
                    ),
                    (
                        RespValue::SimpleString("key2".to_string()),
                        RespValue::Integer(42)
                    ),
                ],
                data: Box::new(RespValue::Array(Some(vec![
                    RespValue::BulkString(Some(Bytes::from("hello"))),
                    RespValue::BulkString(Some(Bytes::from("world"))),
                ])))
            })
        );
    }
}
