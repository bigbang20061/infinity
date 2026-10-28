//! Writer. Emits what the client's own `blue.dll` emits for the same Python
//! values (see crate docs), as a version-0 stream without shared objects.

use crate::dbrow::rle_pack;
use crate::{strings, tag, Value};

/// Encode `v` as a complete marshal stream.
pub fn dumps(v: &Value) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.push(tag::SIGNATURE);
    out.extend_from_slice(&0i32.to_le_bytes()); // shared object count
    write(&mut out, v);
    out
}

/// `WriteStream::WriteInteger`.
fn size(out: &mut Vec<u8>, n: usize) {
    if n < 0xFF {
        out.push(n as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(n as i32).to_le_bytes());
    }
}

fn sized(out: &mut Vec<u8>, t: u8, b: &[u8]) {
    out.push(t);
    size(out, b.len());
    out.extend_from_slice(b);
}

fn int(out: &mut Vec<u8>, i: i64) {
    match i {
        -1 => out.push(tag::INT_N1),
        0 => out.push(tag::INT_0),
        1 => out.push(tag::INT_1),
        _ if i8::try_from(i).is_ok() => {
            out.push(tag::INT8);
            out.push(i as i8 as u8);
        }
        _ if i16::try_from(i).is_ok() => {
            out.push(tag::INT16);
            out.extend_from_slice(&(i as i16).to_le_bytes());
        }
        _ if i32::try_from(i).is_ok() => {
            out.push(tag::INT32);
            out.extend_from_slice(&(i as i32).to_le_bytes());
        }
        // Outside a 32-bit C long this is a Python long: `Marshal::WriteLong`.
        _ => sized(out, tag::LONG, &long_bytes(i)),
    }
}

/// Minimal little-endian two's complement, as `_PyLong_AsByteArray` + trim in WriteLong.
fn long_bytes(i: i64) -> Vec<u8> {
    let mut b = i.to_le_bytes().to_vec();
    while b.len() > 1 {
        let (last, prev) = (b[b.len() - 1], b[b.len() - 2]);
        if (last == 0 && prev & 0x80 == 0) || (last == 0xFF && prev & 0x80 != 0) {
            b.pop();
        } else {
            break;
        }
    }
    b
}

fn seq(out: &mut Vec<u8>, items: &[Value], is_tuple: bool) {
    match (items.len(), is_tuple) {
        (0, true) => out.push(tag::TUPLE0),
        (1, true) => out.push(tag::TUPLE1),
        (2, true) => out.push(tag::TUPLE2),
        (0, false) => out.push(tag::LIST0),
        (1, false) => out.push(tag::LIST1),
        (n, t) => {
            out.push(if t { tag::TUPLE } else { tag::LIST });
            size(out, n);
        }
    }
    for v in items {
        write(out, v);
    }
}

fn write(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::None => out.push(tag::NONE),
        Value::Bool(true) => out.push(tag::TRUE),
        Value::Bool(false) => out.push(tag::FALSE),
        Value::Int(i) => int(out, *i),
        Value::BigLong(b) => sized(out, tag::LONG, b),
        Value::Float(f) if *f == 0.0 && f.is_sign_positive() => out.push(tag::FLOAT_0),
        Value::Float(f) => {
            out.push(tag::FLOAT);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Bytes(b) => match b.len() {
            0 => out.push(tag::STR_EMPTY),
            1 => {
                out.push(tag::STR_CHAR);
                out.push(b[0]);
            }
            _ => match strings::index_of(b) {
                Some(i) => {
                    out.push(tag::STR_TABLE);
                    out.push(i);
                }
                None => sized(out, tag::BUFFER, b),
            },
        },
        Value::Unicode(s) if s.is_empty() => out.push(tag::UNICODE_0),
        Value::Unicode(s) => sized(out, tag::UTF8, s.as_bytes()),
        Value::Tuple(items) => seq(out, items, true),
        Value::List(items) => seq(out, items, false),
        Value::Dict(pairs) => {
            out.push(tag::DICT);
            size(out, pairs.len());
            for (k, v) in pairs {
                write(out, v);
                write(out, k);
            }
        }
        Value::Global(name) => sized(out, tag::GLOBAL, name.as_bytes()),
        Value::Instance(class, state) => {
            out.push(tag::INSTANCE);
            write(out, class);
            write(out, state);
        }
        Value::Reduce { data, list, dict } | Value::NewObj { data, list, dict } => {
            out.push(if matches!(v, Value::Reduce { .. }) { tag::REDUCE } else { tag::NEWOBJ });
            write(out, data);
            for item in list {
                write(out, item);
            }
            out.push(tag::MARK);
            for (k, v) in dict {
                write(out, k);
                write(out, v);
            }
            out.push(tag::MARK);
        }
        Value::DbRow { descriptor, data, objects } => {
            out.push(tag::DBROW);
            write(out, descriptor);
            let packed = rle_pack(data);
            size(out, packed.len());
            out.extend_from_slice(&packed);
            for o in objects {
                write(out, o);
            }
        }
        Value::SubStream(b) => sized(out, tag::WSTREAM, b),
        Value::Callback(v) => {
            out.push(tag::CALLBACK);
            write(out, v);
        }
    }
}
