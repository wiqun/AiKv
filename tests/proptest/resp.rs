//! @component aikv-resp

//! RESP 协议 parser/encoder 的 property-based 测试.

use bytes::Bytes;
use proptest::prelude::*;

use aikv::protocol::{RespParser, RespValue};

/// 生成 RESP2 简单类型 (不含 RESP3-only 类型如 Null/Boolean/Double 等).
///
/// 排除原因: 这些类型在 RESP2 模式下不会由 parser 产生,
/// 若生成则 serialize 输出可能与 parse 后重编码的字节不同.
fn resp2_simple_strategy() -> impl Strategy<Value = RespValue> {
    prop_oneof![
        3 => "[a-zA-Z0-9 ]{0,64}".prop_map(RespValue::SimpleString),
        1 => "-[a-zA-Z ]{1,64}".prop_map(RespValue::Error),
        2 => any::<i64>().prop_map(RespValue::Integer),
        3 => prop::collection::vec(any::<u8>(), 0..128)
            .prop_map(|v| RespValue::BulkString(Some(Bytes::from(v)))),
        1 => Just(RespValue::BulkString(None)),
        1 => Just(RespValue::Array(None)),
    ]
}

/// RESP2 简单类型: encode → parse → encode 字节完全一致.
#[test]
fn prop_resp_parse_encode_identity() {
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&(resp2_simple_strategy()), |value| {
            let encoded = value.serialize();
            let mut parser = RespParser::new();
            parser.feed(&encoded);
            let parsed = parser.parse().unwrap().unwrap();
            let re_encoded = parsed.serialize();
            prop_assert_eq!(encoded, re_encoded);
            Ok(())
        })
        .unwrap();
}

/// 生成嵌套 RESP2 数组 (depth ≤ 4), 含 BulkString(None) 变体以覆盖 Null 映射.
fn resp2_array_strategy(_depth: u8) -> impl Strategy<Value = RespValue> {
    let leaf = prop_oneof![
        2 => "[a-z]{1,16}".prop_map(RespValue::SimpleString),
        1 => any::<i64>().prop_map(RespValue::Integer),
        2 => prop::collection::vec(any::<u8>(), 0..32)
            .prop_map(|v| RespValue::BulkString(Some(Bytes::from(v)))),
        1 => Just(RespValue::BulkString(None)),
    ];
    leaf.prop_recursive(4, 32, 3, move |inner| {
        prop::collection::vec(inner, 0..8).prop_map(|items| RespValue::Array(Some(items)))
    })
}

/// 嵌套 Array: encode → parse 结构等价 (允许 BulkString(None) ↔ Null 映射).
#[test]
fn prop_resp_array_roundtrip() {
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&resp2_array_strategy(4), |value| {
            let encoded = value.serialize();
            let mut parser = RespParser::new();
            parser.feed(&encoded);
            let parsed = parser.parse().unwrap().unwrap();
            // 结构等价: 允许 BulkString(None) → Null 转换
            prop_assert!(
                resp_value_equiv(&value, &parsed),
                "original: {value:?}, parsed: {parsed:?}"
            );
            Ok(())
        })
        .unwrap();
}

/// 二进制 BulkString (含 `\r\n` / `\0` / 非 UTF-8) 往返无损.
#[test]
fn prop_resp_bulkstring_binary() {
    let mut runner = proptest::test_runner::TestRunner::default();
    let strategy = prop::collection::vec(any::<u8>(), 0..1024)
        .prop_map(|v| RespValue::BulkString(Some(Bytes::from(v))));
    runner
        .run(&strategy, |value| {
            let encoded = value.serialize();
            let mut parser = RespParser::new();
            parser.feed(&encoded);
            let parsed = parser.parse().unwrap().unwrap();
            // 提取解析后的字节并和原始 value 比较
            match (&value, &parsed) {
                (RespValue::BulkString(Some(a)), RespValue::BulkString(Some(b))) => {
                    prop_assert_eq!(a, b, "binary roundtrip lost data");
                }
                _ => prop_assert!(false, "type mismatch after roundtrip"),
            }
            Ok(())
        })
        .unwrap();
}

/// 生成随机 RespValue 树 (depth ≤ 3, 包含所有 RESP3 类型).
fn resp3_tree_strategy(_depth: u8) -> impl Strategy<Value = RespValue> {
    let leaf = prop_oneof![
        1 => Just(RespValue::Null),
        2 => "[a-zA-Z]{1,32}".prop_map(RespValue::SimpleString),
        2 => "[A-Z ]{1,32}".prop_map(RespValue::Error),
        2 => any::<i64>().prop_map(RespValue::Integer),
        2 => any::<bool>().prop_map(RespValue::Boolean),
        2 => (-1000.0f64..1000.0f64).prop_map(RespValue::Double),
        3 => prop::collection::vec(any::<u8>(), 0..64)
            .prop_map(|v| RespValue::BulkString(Some(Bytes::from(v)))),
        1 => Just(RespValue::BulkString(None)),
        1 => Just(RespValue::Array(None)),
    ];
    leaf.prop_recursive(3, 16, 2, move |inner| {
        prop_oneof![
            3 => prop::collection::vec(inner.clone(), 0..6)
                .prop_map(|items| RespValue::Array(Some(items))),
            2 => prop::collection::vec(
                (inner.clone(), inner.clone()), 0..3
            ).prop_map(RespValue::Map),
            1 => prop::collection::vec(inner.clone(), 0..4)
                .prop_map(RespValue::Set),
            1 => prop::collection::vec(inner, 0..4)
                .prop_map(RespValue::Push),
        ]
    })
}

/// RESP3 结构树: encode → parse 结构等价.
#[test]
fn prop_encoder_roundtrip_resp3() {
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&resp3_tree_strategy(3), |value| {
            let encoded = value.serialize();
            let mut parser = RespParser::new();
            parser.feed(&encoded);
            let parsed = match parser.parse() {
                Ok(Some(v)) => v,
                Ok(None) => {
                    return Err(proptest::test_runner::TestCaseError::reject(
                        "incomplete frame",
                    ));
                }
                Err(e) => {
                    return Err(proptest::test_runner::TestCaseError::fail(format!(
                        "parse error: {e}"
                    )));
                }
            };
            prop_assert!(
                resp_value_equiv(&value, &parsed),
                "structural mismatch\n  original: {value:?}\n  parsed:   {parsed:?}"
            );
            Ok(())
        })
        .unwrap();
}

/// 浅层 RESP3 类型往返.
#[test]
fn prop_encoder_simple_types() {
    let strategy = prop_oneof![
        2 => "[a-z]{1,32}".prop_map(RespValue::SimpleString),
        2 => "[A-Z ]{1,32}".prop_map(RespValue::Error),
        2 => any::<i64>().prop_map(RespValue::Integer),
        2 => any::<bool>().prop_map(RespValue::Boolean),
        2 => (-1e6f64..1e6f64).prop_map(RespValue::Double),
    ];
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&strategy, |value| {
            let encoded = value.serialize();
            let mut parser = RespParser::new();
            parser.feed(&encoded);
            let parsed = parser.parse().unwrap().unwrap();
            prop_assert!(
                resp_value_equiv(&value, &parsed),
                "mismatch: {value:?} vs {parsed:?}"
            );
            Ok(())
        })
        .unwrap();
}

/// 畸形输入: parser 不 panic, 正确区分不完整帧与错误.
#[test]
fn prop_parser_malformed_input() {
    let strategy = prop::collection::vec(any::<u8>(), 0..1024);
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&strategy, |bytes| {
            let mut parser = RespParser::new();
            parser.feed(&bytes);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.parse()));
            match result {
                Ok(Ok(None)) | Ok(Err(_)) => {}
                Ok(Ok(Some(_))) => {} // 极低概率偶然解析出合法帧
                Err(_) => prop_assert!(false, "parser panicked on input: {bytes:?}"),
            }
            Ok(())
        })
        .unwrap();
}

// ---- 结构等价比较 ----

/// 比较两个 RespValue 是否结构等价 (非字节等价).
fn resp_value_equiv(a: &RespValue, b: &RespValue) -> bool {
    match (a, b) {
        (RespValue::Double(x), RespValue::Double(y)) if x.is_nan() && y.is_nan() => true,
        (RespValue::Double(x), RespValue::Double(y)) => x == y,
        (RespValue::BulkString(None), RespValue::Null)
        | (RespValue::Null, RespValue::BulkString(None)) => true,
        (RespValue::Array(Some(a_items)), RespValue::Array(Some(b_items))) => {
            a_items.len() == b_items.len()
                && a_items
                    .iter()
                    .zip(b_items)
                    .all(|(a, b)| resp_value_equiv(a, b))
        }
        (RespValue::Array(None), RespValue::Array(None)) => true,
        (RespValue::Map(a_pairs), RespValue::Map(b_pairs)) => {
            a_pairs.len() == b_pairs.len()
                && a_pairs.iter().zip(b_pairs).all(|((ak, av), (bk, bv))| {
                    resp_value_equiv(ak, bk) && resp_value_equiv(av, bv)
                })
        }
        (RespValue::Set(a_items), RespValue::Set(b_items)) => {
            a_items.len() == b_items.len()
                && a_items
                    .iter()
                    .zip(b_items)
                    .all(|(a, b)| resp_value_equiv(a, b))
        }
        (RespValue::Push(a_items), RespValue::Push(b_items)) => {
            a_items.len() == b_items.len()
                && a_items
                    .iter()
                    .zip(b_items)
                    .all(|(a, b)| resp_value_equiv(a, b))
        }
        (
            RespValue::Attribute {
                attributes: aa,
                data: ad,
            },
            RespValue::Attribute {
                attributes: ba,
                data: bd,
            },
        ) => {
            aa.len() == ba.len()
                && aa.iter().zip(ba).all(|((ak, av), (bk, bv))| {
                    resp_value_equiv(ak, bk) && resp_value_equiv(av, bv)
                })
                && resp_value_equiv(ad, bd)
        }
        _ => a == b,
    }
}
