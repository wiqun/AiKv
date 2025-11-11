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
            },
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
            b'+' => self.parse_simple_string(cursor),
            b'-' => self.parse_error(cursor),
            b':' => self.parse_integer(cursor),
            b'$' => self.parse_bulk_string(cursor),
            b'*' => self.parse_array(cursor),
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
}
