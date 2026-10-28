//! CCP `blue.marshal` codec for the Infinity client (carbon-blue v2.6.5,
//! Python 2.7, `PY27_COMPATIBILITY_MODE`).
//!
//! Source of truth: carbonengine/blue `Marshal.cpp` / `Marshal.h` /
//! `PyRowSet.cpp`, cross-checked by parsing files written by the client's own
//! `blue.dll` (byte-identical between the Infinity and Serenity builds):
//!
//! * Python 2 `str` is written as `TY_BUFFER` (0x13), or the empty / single
//!   char / string-table short forms. `TY_STR*` long forms are only read.
//! * `unicode` is written as `TY_UTF8` (0x2E). `TY_UNICODE` (0x12) is read as
//!   UTF-8, matching carbonengine.
//! * Integers outside the 32-bit C `long` range are Python longs: `TY_LONG`.
//! * Stream header is version 0: `0x7E`, i32 shared count, objects, then the
//!   shared-object index map at the very end of the buffer.

mod dbrow;
mod decode;
mod encode;
pub mod strings;

pub use dbrow::{DbRowDescriptor, DbType};
pub use decode::loads;
pub use encode::dumps;

use std::fmt;

/// Type tags (`PYTYPES` in Marshal.h).
pub mod tag {
    pub const SIGNATURE: u8 = 0x7E;
    pub const SIGNATURE2: u8 = 0x7D;
    pub const NONE: u8 = 1;
    pub const GLOBAL: u8 = 2;
    pub const INT64: u8 = 3;
    pub const INT32: u8 = 4;
    pub const INT16: u8 = 5;
    pub const INT8: u8 = 6;
    pub const INT_N1: u8 = 7;
    pub const INT_0: u8 = 8;
    pub const INT_1: u8 = 9;
    pub const FLOAT: u8 = 10;
    pub const FLOAT_0: u8 = 11;
    pub const COMPLEX: u8 = 12;
    pub const STR: u8 = 13;
    pub const STR_EMPTY: u8 = 14;
    pub const STR_CHAR: u8 = 15;
    pub const STR_SHORT: u8 = 16;
    pub const STR_TABLE: u8 = 17;
    pub const UNICODE: u8 = 18;
    pub const BUFFER: u8 = 19;
    pub const TUPLE: u8 = 20;
    pub const LIST: u8 = 21;
    pub const DICT: u8 = 22;
    pub const INSTANCE: u8 = 23;
    pub const CALLBACK: u8 = 25;
    pub const PICKLE: u8 = 26;
    pub const REFERENCE: u8 = 27;
    pub const CRC_CHECK: u8 = 28;
    pub const TRUE: u8 = 31;
    pub const FALSE: u8 = 32;
    pub const PICKLER: u8 = 33;
    pub const REDUCE: u8 = 34;
    pub const NEWOBJ: u8 = 35;
    pub const TUPLE0: u8 = 36;
    pub const TUPLE1: u8 = 37;
    pub const LIST0: u8 = 38;
    pub const LIST1: u8 = 39;
    pub const UNICODE_0: u8 = 40;
    pub const UNICODE_1: u8 = 41;
    pub const DBROW: u8 = 42;
    pub const WSTREAM: u8 = 43;
    pub const TUPLE2: u8 = 44;
    pub const MARK: u8 = 45;
    pub const UTF8: u8 = 46;
    pub const LONG: u8 = 47;
    pub const SHARED_FLAG: u8 = 0x40;
    pub const TYPE_MASK: u8 = 0x3F;
}

/// A decoded Python object.
#[derive(Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    /// Python long that does not fit in i64: little-endian two's complement.
    BigLong(Vec<u8>),
    Float(f64),
    /// Python 2 `str` (byte string).
    Bytes(Vec<u8>),
    Unicode(String),
    Tuple(Vec<Value>),
    List(Vec<Value>),
    /// Insertion-ordered key/value pairs.
    Dict(Vec<(Value, Value)>),
    /// Class or function referenced by `module.name`.
    Global(String),
    /// Old-style class instance: `(class name, state)`.
    Instance(Box<Value>, Box<Value>),
    /// `__reduce__` protocol: `data` is `(callable, args[, state])`.
    Reduce { data: Box<Value>, list: Vec<Value>, dict: Vec<(Value, Value)> },
    /// `copy_reg.__newobj__` form: `data` is `((cls, *args)[, state])`.
    NewObj { data: Box<Value>, list: Vec<Value>, dict: Vec<(Value, Value)> },
    /// `blue.DBRow`: descriptor object, unpacked fixed-size data, trailing objects.
    DbRow { descriptor: Box<Value>, data: Vec<u8>, objects: Vec<Value> },
    /// `blue.MarshalStream`: an embedded, already-marshalled stream.
    SubStream(Vec<u8>),
    /// `TY_CALLBACK`: the marshal callback's substitute for an object, e.g. a
    /// machoNet bound-object reference (`ObjectCallGPCS.GetObjectID`).
    Callback(Box<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated { at: usize, wanted: usize },
    BadSignature(u8),
    BadTag { tag: u8, at: usize },
    BadStringIndex(u8),
    BadReference(i32),
    BadSharedMap(String),
    BadUtf8 { at: usize },
    BadRle,
    TooDeep,
    Unsupported(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { at, wanted } => write!(f, "truncated stream: wanted {wanted} bytes at {at}"),
            Error::BadSignature(b) => write!(f, "bad marshal signature 0x{b:02x}"),
            Error::BadTag { tag, at } => write!(f, "invalid type tag {tag} at offset {at}"),
            Error::BadStringIndex(i) => write!(f, "invalid string table index {i}"),
            Error::BadReference(i) => write!(f, "invalid TY_REFERENCE {i}"),
            Error::BadSharedMap(m) => write!(f, "bad shared object map: {m}"),
            Error::BadUtf8 { at } => write!(f, "invalid UTF-8 at offset {at}"),
            Error::BadRle => write!(f, "invalid DBRow RLE data"),
            Error::TooDeep => write!(f, "recursion limit exceeded"),
            Error::Unsupported(what) => write!(f, "unsupported marshal feature: {what}"),
        }
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------- builders
impl Value {
    /// Python 2 `str`.
    pub fn str(s: impl AsRef<[u8]>) -> Value {
        Value::Bytes(s.as_ref().to_vec())
    }
    pub fn unicode(s: impl Into<String>) -> Value {
        Value::Unicode(s.into())
    }
    pub fn tuple(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Tuple(items.into_iter().collect())
    }
    pub fn list(items: impl IntoIterator<Item = Value>) -> Value {
        Value::List(items.into_iter().collect())
    }
    /// Dict with Python 2 `str` keys, the common case on the wire.
    pub fn dict<K: AsRef<[u8]>>(items: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Dict(items.into_iter().map(|(k, v)| (Value::str(k), v)).collect())
    }
    pub fn global(name: impl Into<String>) -> Value {
        Value::Global(name.into())
    }
    /// Old-style class instance with a dict state.
    pub fn instance<K: AsRef<[u8]>>(class: &str, state: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Instance(Box::new(Value::str(class)), Box::new(Value::dict(state)))
    }
    /// `callable(*args)` via the reduce protocol, optionally followed by `__setstate__(state)`.
    pub fn reduce(callable: &str, args: Vec<Value>, state: Option<Value>) -> Value {
        let mut data = vec![Value::global(callable), Value::Tuple(args)];
        data.extend(state);
        Value::Reduce { data: Box::new(Value::Tuple(data)), list: vec![], dict: vec![] }
    }
}

// ---------------------------------------------------------------- accessors
impl Value {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    /// Text from either string flavour (lossy for non-UTF-8 byte strings).
    pub fn as_text(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Value::Bytes(b) => Some(String::from_utf8_lossy(b)),
            Value::Unicode(s) => Some(s.as_str().into()),
            _ => None,
        }
    }
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) | Value::SubStream(b) => Some(b),
            _ => None,
        }
    }
    /// Elements of a tuple, list, or Python set (pickled as `Reduce(set, ([items],))`).
    pub fn items(&self) -> Option<&[Value]> {
        match self {
            Value::Tuple(v) | Value::List(v) => Some(v),
            Value::Reduce { data, .. } => match data.items()? {
                [Value::Global(g), args] if g == "__builtin__.set" || g == "__builtin__.frozenset" => match args.items()? {
                    [inner] => inner.items(),
                    [] => Some(&[]),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }
    /// Dict lookup by string key (either flavour).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == key)).map(|(_, v)| v),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- repr
impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn seq(f: &mut fmt::Formatter<'_>, open: &str, items: &[Value], close: &str) -> fmt::Result {
            f.write_str(open)?;
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{v:?}")?;
            }
            if open == "(" && items.len() == 1 {
                f.write_str(",")?;
            }
            f.write_str(close)
        }
        fn pairs(f: &mut fmt::Formatter<'_>, items: &[(Value, Value)]) -> fmt::Result {
            f.write_str("{")?;
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{k:?}: {v:?}")?;
            }
            f.write_str("}")
        }
        match self {
            Value::None => f.write_str("None"),
            Value::Bool(true) => f.write_str("True"),
            Value::Bool(false) => f.write_str("False"),
            Value::Int(i) => write!(f, "{i}"),
            Value::BigLong(b) => write!(f, "long(0x{})", b.iter().rev().map(|x| format!("{x:02x}")).collect::<String>()),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Bytes(b) => match std::str::from_utf8(b) {
                Ok(s) if b.iter().all(|c| (0x20..0x7f).contains(c)) => write!(f, "'{s}'"),
                _ => write!(f, "'{}'", b.escape_ascii()),
            },
            Value::Unicode(s) => write!(f, "u{s:?}"),
            Value::Tuple(v) => seq(f, "(", v, ")"),
            Value::List(v) => seq(f, "[", v, "]"),
            Value::Dict(p) => pairs(f, p),
            Value::Global(g) => write!(f, "<{g}>"),
            Value::Instance(c, s) => write!(f, "Instance({c:?}, {s:?})"),
            Value::Reduce { data, list, dict } | Value::NewObj { data, list, dict } => {
                let kind = if matches!(self, Value::Reduce { .. }) { "Reduce" } else { "NewObj" };
                write!(f, "{kind}{data:?}")?;
                if !list.is_empty() {
                    seq(f, "+", list, "")?;
                }
                if !dict.is_empty() {
                    f.write_str("+")?;
                    pairs(f, dict)?;
                }
                Ok(())
            }
            Value::DbRow { descriptor, data, objects } => {
                write!(f, "DBRow({descriptor:?}, {} bytes, ", data.len())?;
                seq(f, "[", objects, "])")
            }
            Value::Callback(v) => write!(f, "Callback({v:?})"),
            Value::SubStream(b) => match loads(b) {
                Ok(v) => write!(f, "Stream({v:?})"),
                Err(_) => write!(f, "Stream({} bytes)", b.len()),
            },
        }
    }
}

#[cfg(test)]
mod tests;
