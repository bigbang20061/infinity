//! `blue.DBRowDescriptor` / `blue.DBRow` (carbonengine/blue PyRowSet.cpp).

use crate::{Error, Value};

/// ADO DBTYPE codes accepted by `ColumnDescriptor::TypeSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DbType {
    Empty = 0,
    I2 = 2,
    I4 = 3,
    R4 = 4,
    R8 = 5,
    Cy = 6,
    Bool = 11,
    I1 = 16,
    UI1 = 17,
    UI2 = 18,
    UI4 = 19,
    I8 = 20,
    UI8 = 21,
    FileTime = 64,
    Bytes = 128,
    Str = 129,
    WStr = 130,
    DbTimestamp = 135,
}

impl DbType {
    pub fn from_code(c: i64) -> Option<DbType> {
        use DbType::*;
        Some(match c {
            0 => Empty,
            2 => I2,
            3 => I4,
            4 => R4,
            5 => R8,
            6 => Cy,
            11 => Bool,
            16 => I1,
            17 => UI1,
            18 => UI2,
            19 => UI4,
            20 => I8,
            21 => UI8,
            64 => FileTime,
            128 => Bytes,
            129 => Str,
            130 => WStr,
            135 => DbTimestamp,
            _ => return None,
        })
    }

    /// `TypeSize`: log2(bytes)+1 for fixed columns, 0 for bool, 5 for objects.
    fn size_class(self) -> Option<usize> {
        use DbType::*;
        match self {
            Bool => Some(0),
            I1 | UI1 => Some(1),
            I2 | UI2 => Some(2),
            I4 | UI4 | R4 => Some(3),
            I8 | UI8 | R8 | Cy | FileTime | DbTimestamp => Some(4),
            Bytes | Str | WStr => Some(5),
            Empty => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Byte offset and width inside the fixed data block.
    Fixed { offset: usize, width: usize },
    /// Bit offset inside the fixed data block.
    Bit(usize),
    /// Index into the trailing object list.
    Object(usize),
    Virtual,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub slots: Vec<Slot>,
    /// Bit offset of the per-column null flags.
    pub null_bits: usize,
    pub data_len: usize,
    pub n_objects: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DbRowDescriptor {
    pub columns: Vec<(String, DbType)>,
    /// Virtual columns: (name, global function computing it from the row), e.g.
    /// ('stacksize', 'eve.common.script.sys.eveCfg.StackSize'). Not stored in row data.
    pub virtual_columns: Vec<(String, String)>,
}

impl DbRowDescriptor {
    pub fn new<S: Into<String>>(columns: impl IntoIterator<Item = (S, DbType)>) -> Self {
        DbRowDescriptor { columns: columns.into_iter().map(|(n, t)| (n.into(), t)).collect(), virtual_columns: vec![] }
    }

    pub fn with_virtual<S: Into<String>, F: Into<String>>(mut self, v: impl IntoIterator<Item = (S, F)>) -> Self {
        self.virtual_columns = v.into_iter().map(|(n, f)| (n.into(), f.into())).collect();
        self
    }

    /// `RowDescriptor::InitFromTypedList`: fixed columns grouped largest first,
    /// then bools (bits), then one null bit per real column, then objects.
    pub fn layout(&self) -> Layout {
        let mut sizes = [0usize; 5];
        let mut n_objects = 0;
        let mut n_real = 0;
        for (_, t) in &self.columns {
            match t.size_class() {
                Some(5) => {
                    n_objects += 1;
                    n_real += 1;
                }
                Some(c) => {
                    sizes[c] += 1;
                    n_real += 1;
                }
                None => {}
            }
        }
        let mut offsets = [0usize; 5];
        let mut offset = 0;
        for c in (1..=4).rev() {
            offsets[c] = offset;
            offset += sizes[c] * (1 << (c - 1));
        }
        offset *= 8;
        offsets[0] = offset;
        offset += sizes[0];
        let null_bits = offset;
        offset += n_real;
        let data_len = offset.div_ceil(8);

        let mut next_object = 0;
        let slots = self
            .columns
            .iter()
            .map(|(_, t)| match t.size_class() {
                Some(5) => {
                    next_object += 1;
                    Slot::Object(next_object - 1)
                }
                Some(0) => {
                    offsets[0] += 1;
                    Slot::Bit(offsets[0] - 1)
                }
                Some(c) => {
                    let width = 1 << (c - 1);
                    offsets[c] += width;
                    Slot::Fixed { offset: offsets[c] - width, width }
                }
                None => Slot::Virtual,
            })
            .collect();
        Layout { slots, null_bits, data_len, n_objects }
    }

    /// Wire form: `__reduce_ex__` -> `(blue.DBRowDescriptor, (((name, type), ...),))`.
    pub fn to_value(&self) -> Value {
        let cols = self.columns.iter().map(|(n, t)| Value::tuple([Value::str(n), Value::Int(*t as i64)]));
        let state = (!self.virtual_columns.is_empty()).then(|| {
            Value::list(self.virtual_columns.iter().map(|(n, f)| Value::tuple([Value::str(n), Value::global(f.clone())])))
        });
        Value::reduce("blue.DBRowDescriptor", vec![Value::tuple(cols)], state)
    }

    pub fn from_value(v: &Value) -> Option<Self> {
        let Value::Reduce { data, .. } = v else { return None };
        let parts = data.items()?;
        match parts.first()? {
            Value::Global(g) if g == "blue.DBRowDescriptor" => {}
            _ => return None,
        }
        let cols = parts.get(1)?.items()?.first()?.items()?;
        let columns = cols
            .iter()
            .map(|c| {
                let c = c.items()?;
                Some((c.first()?.as_text()?.into_owned(), DbType::from_code(c.get(1)?.as_int()?)?))
            })
            .collect::<Option<Vec<_>>>()?;
        let virtual_columns = parts
            .get(2)
            .and_then(|v| v.items())
            .map(|vs| {
                vs.iter()
                    .filter_map(|t| {
                        let t = t.items()?;
                        let f = match t.get(1)? {
                            Value::Global(g) => g.clone(),
                            _ => return None,
                        };
                        Some((t.first()?.as_text()?.into_owned(), f))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(DbRowDescriptor { columns, virtual_columns })
    }

    /// Build a row; `values` correspond to columns in order (`Value::None` = NULL).
    pub fn row(&self, values: &[Value]) -> Value {
        let layout = self.layout();
        let mut data = vec![0u8; layout.data_len];
        let mut objects = vec![Value::None; layout.n_objects];
        for (i, ((_, t), slot)) in self.columns.iter().zip(&layout.slots).enumerate() {
            let v = values.get(i).unwrap_or(&Value::None);
            if v.is_none() && *slot != Slot::Virtual {
                set_bit(&mut data, layout.null_bits + i);
                continue;
            }
            match *slot {
                Slot::Bit(bit) => {
                    if v.as_int().unwrap_or(0) != 0 {
                        set_bit(&mut data, bit);
                    }
                }
                Slot::Fixed { offset, width } => {
                    let bytes: [u8; 8] = match t {
                        DbType::R4 => {
                            let mut b = [0; 8];
                            b[..4].copy_from_slice(&(v.as_float().unwrap_or(0.0) as f32).to_le_bytes());
                            b
                        }
                        DbType::R8 => v.as_float().unwrap_or(0.0).to_le_bytes(),
                        DbType::Cy => match v {
                            Value::Float(f) => ((f * 10000.0).round() as i64).to_le_bytes(),
                            _ => v.as_int().unwrap_or(0).to_le_bytes(),
                        },
                        _ => v.as_int().unwrap_or(0).to_le_bytes(),
                    };
                    data[offset..offset + width].copy_from_slice(&bytes[..width]);
                }
                Slot::Object(idx) => objects[idx] = v.clone(),
                Slot::Virtual => {}
            }
        }
        Value::DbRow { descriptor: Box::new(self.to_value()), data, objects }
    }
}

fn set_bit(data: &mut [u8], bit: usize) {
    data[bit >> 3] |= 1 << (bit & 7);
}

/// `RLEPack`: each nibble is a run; 8..=15 = (n-7) zero bytes, 0..=7 = (8-n) literal bytes.
pub(crate) fn rle_pack(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 8 + 1);
    let mut zerorun = 0;
    let mut nibble = false;
    let mut marker = 0;
    let mut i = 0;
    while i < data.len() {
        if !nibble {
            marker = out.len();
            out.push(0u8);
        }
        let start = i;
        let end = (i + 8).min(data.len());
        let count: i32 = if data[i] == 0 {
            zerorun += 1;
            i += 1;
            while i < end && data[i] == 0 {
                i += 1;
            }
            (i - start - 1) as i32
        } else {
            zerorun = 0;
            out.push(data[i]);
            i += 1;
            while i < end && data[i] != 0 {
                out.push(data[i]);
                i += 1;
            }
            start as i32 - i as i32
        };
        let mut count = (count + 8) as u8 & 0x0F;
        if nibble {
            count <<= 4;
        }
        out[marker] |= count;
        nibble = !nibble;
    }
    if nibble && zerorun > 0 {
        zerorun += 1; // implicit last nibble is also zero
    }
    while zerorun >= 2 {
        out.pop();
        zerorun -= 2;
    }
    out
}

/// `RLEUnPack` into exactly `out_len` bytes (missing tail is zero).
pub(crate) fn rle_unpack(data: &[u8], out_len: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![0u8; out_len];
    let mut outc = 0;
    let mut nibble = false;
    let mut run = 0u8;
    let mut i = 0;
    while i < data.len() && outc < out_len {
        let count = if !nibble {
            run = data[i];
            i += 1;
            run & 0x0F
        } else {
            run >> 4
        } as i32
            - 8;
        nibble = !nibble;
        if count >= 0 {
            let n = count as usize + 1;
            if outc + n > out_len {
                return Err(Error::BadRle);
            }
            outc += n; // already zero
        } else {
            let n = (-count) as usize;
            if outc + n > out_len {
                return Err(Error::BadRle);
            }
            for _ in 0..n {
                if i >= data.len() {
                    break;
                }
                out[outc] = data[i];
                outc += 1;
                i += 1;
            }
        }
    }
    Ok(out)
}
