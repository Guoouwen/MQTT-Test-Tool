//! protobuf wire format 编码器。
//!
//! 创建时间：2026-11-17
//! 修改时间：2026-11-17（新增，支持字段表格可视化编码，替代手填 hex）
//!
//! 把前端「字段表格」（字段号 + 类型 + 值）逐行编码为 protobuf 二进制：
//!   tag = (field_number << 3) | wire_type，后接按 wire_type 编码的 value。
//! 支持标量类型，嵌套 message / repeated 暂不支持。

use serde::Deserialize;

/// 前端字段表格里的一行。
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProtoField {
    pub field_number: u32,
    #[serde(rename = "type")]
    pub typ: String,
    pub value: String,
}

/// 写一个 varint（base-128 变长编码）到缓冲区。
fn write_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        buf.push(b);
        if v == 0 {
            break;
        }
    }
}

/// 单个整数编码为 varint 字节。
fn varint_bytes(v: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(8);
    write_varint(&mut b, v);
    b
}

fn parse_i64(s: &str, typ: &str) -> Result<i64, String> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| format!("字段类型 {typ} 的值「{s}」不是合法整数"))
}

fn parse_u64(s: &str, typ: &str) -> Result<u64, String> {
    s.trim()
        .parse::<u64>()
        .map_err(|_| format!("字段类型 {typ} 的值「{s}」不是合法非负整数"))
}

/// sint32 的 zigzag 编码（负数也能紧凑表示）。
fn zigzag32(v: i64) -> u64 {
    (((v << 1) ^ (v >> 31)) as u32) as u64
}

/// sint64 的 zigzag 编码。
fn zigzag64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// 将 hex 字符串解码为字节；非法时退回按 UTF-8 文本。
pub fn decode_hex(s: &str) -> Vec<u8> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let cleaned = cleaned.trim_start_matches("0x").trim_start_matches("0X");
    if cleaned.is_empty() || cleaned.len() % 2 != 0 {
        return s.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push(((h as u8) << 4) | (l as u8)),
            _ => return s.as_bytes().to_vec(),
        }
        i += 2;
    }
    out
}

/// 把字段列表编码为 protobuf 二进制字节。
pub fn encode_proto(fields: &[ProtoField]) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    for f in fields {
        let (wire_type, payload): (u32, Vec<u8>) = match f.typ.as_str() {
            // wire type 0：varint
            "int32" | "int64" => {
                let v = parse_i64(&f.value, &f.typ)?;
                (0, varint_bytes(v as u64))
            }
            "uint32" | "uint64" | "enum" => {
                let v = parse_u64(&f.value, &f.typ)?;
                (0, varint_bytes(v))
            }
            "sint32" => {
                let v = parse_i64(&f.value, &f.typ)?;
                (0, varint_bytes(zigzag32(v)))
            }
            "sint64" => {
                let v = parse_i64(&f.value, &f.typ)?;
                (0, varint_bytes(zigzag64(v)))
            }
            "bool" => {
                let v = match f.value.trim().to_lowercase().as_str() {
                    "true" | "1" | "yes" => 1u64,
                    "false" | "0" | "no" | "" => 0,
                    _ => {
                        return Err(format!(
                            "字段类型 bool 的值「{}」无效，填 true/false/1/0",
                            f.value
                        ))
                    }
                };
                (0, varint_bytes(v))
            }
            // wire type 2：length-delimited
            "string" => {
                let b = f.value.as_bytes();
                let mut v = Vec::with_capacity(b.len() + 8);
                write_varint(&mut v, b.len() as u64);
                v.extend_from_slice(b);
                (2, v)
            }
            "bytes" => {
                let b = decode_hex(&f.value);
                let mut v = Vec::with_capacity(b.len() + 8);
                write_varint(&mut v, b.len() as u64);
                v.extend_from_slice(&b);
                (2, v)
            }
            // wire type 5：32-bit 小端
            "float" => {
                let x: f32 = f
                    .value
                    .trim()
                    .parse()
                    .map_err(|_| format!("字段类型 float 的值「{}」不是合法小数", f.value))?;
                (5, x.to_le_bytes().to_vec())
            }
            "fixed32" | "sfixed32" => {
                let v = parse_i64(&f.value, &f.typ)?;
                (5, (v as u32).to_le_bytes().to_vec())
            }
            // wire type 1：64-bit 小端
            "double" => {
                let x: f64 = f
                    .value
                    .trim()
                    .parse()
                    .map_err(|_| format!("字段类型 double 的值「{}」不是合法小数", f.value))?;
                (1, x.to_le_bytes().to_vec())
            }
            "fixed64" | "sfixed64" => {
                let v = parse_i64(&f.value, &f.typ)?;
                (1, (v as u64).to_le_bytes().to_vec())
            }
            other => return Err(format!("不支持的 protobuf 类型「{other}」")),
        };
        // 写 tag：field_number 左移 3 位 + wire_type
        write_varint(&mut buf, ((f.field_number as u64) << 3) | (wire_type as u64));
        buf.extend_from_slice(&payload);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(num: u32, typ: &str, value: &str) -> ProtoField {
        ProtoField {
            field_number: num,
            typ: typ.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn encode_string() {
        // field 1, string "hello" => 0a 05 68 65 6c 6c 6f
        let out = encode_proto(&[field(1, "string", "hello")]).unwrap();
        assert_eq!(out, vec![0x0a, 0x05, 0x68, 0x65, 0x6c, 0x6c, 0x6f]);
    }

    #[test]
    fn encode_int32_varint() {
        // field 1, int32 150 => 08 96 01
        let out = encode_proto(&[field(1, "int32", "150")]).unwrap();
        assert_eq!(out, vec![0x08, 0x96, 0x01]);
    }

    #[test]
    fn encode_sint32_negative() {
        // field 1, sint32 -1 => zigzag 1 => 08 01
        let out = encode_proto(&[field(1, "sint32", "-1")]).unwrap();
        assert_eq!(out, vec![0x08, 0x01]);
    }

    #[test]
    fn encode_bool() {
        // field 1, bool true => 08 01
        let out = encode_proto(&[field(1, "bool", "true")]).unwrap();
        assert_eq!(out, vec![0x08, 0x01]);
    }

    #[test]
    fn encode_multiple_fields() {
        // field 1 string "hi", field 2 int32 1 => 0a 02 68 69 10 01
        let out = encode_proto(&[
            field(1, "string", "hi"),
            field(2, "int32", "1"),
        ])
        .unwrap();
        assert_eq!(out, vec![0x0a, 0x02, 0x68, 0x69, 0x10, 0x01]);
    }
}
