use crate::dbrow::{rle_pack, rle_unpack, Slot};
use crate::*;

fn roundtrip(v: Value) {
    let b = dumps(&v);
    assert_eq!(loads(&b).unwrap(), v, "bytes: {b:02x?}");
}

#[test]
fn scalars_roundtrip() {
    for i in [0, 1, -1, 2, -128, 127, 128, -32768, 32767, 40000, i32::MIN as i64, i32::MAX as i64, 1 << 40, i64::MIN, i64::MAX] {
        roundtrip(Value::Int(i));
    }
    roundtrip(Value::None);
    roundtrip(Value::Bool(true));
    roundtrip(Value::Bool(false));
    roundtrip(Value::Float(0.0));
    roundtrip(Value::Float(-1.5e300));
    roundtrip(Value::str(""));
    roundtrip(Value::str("x"));
    roundtrip(Value::str("macho.CallReq"));
    roundtrip(Value::str("not in the table"));
    roundtrip(Value::str(vec![0xFFu8; 300]));
    roundtrip(Value::unicode(""));
    roundtrip(Value::unicode("吉他 Jita"));
}

#[test]
fn client_byte_forms() {
    // Shapes observed in files written by the client's blue.dll.
    assert_eq!(&dumps(&Value::str("localization"))[5..], b"\x13\x0clocalization");
    assert_eq!(&dumps(&Value::str("macho.CallReq"))[5..], &[tag::STR_TABLE, 46]);
    assert_eq!(&dumps(&Value::unicode("ab"))[5..], b"\x2e\x02ab");
    assert_eq!(&dumps(&Value::Int(1 << 40))[5..], &[tag::LONG, 6, 0, 0, 0, 0, 0, 1]);
    assert_eq!(&dumps(&Value::Int(170472))[5..], &[tag::INT32, 0xE8, 0x99, 0x02, 0x00]);
    assert_eq!(&dumps(&Value::None)[..], &[0x7E, 0, 0, 0, 0, 1]);
}

#[test]
fn long_widths() {
    // WriteLong uses nbits/8+1 bytes: i64::MAX arrives as 9 bytes.
    let mut nine = i64::MAX.to_le_bytes().to_vec();
    nine.push(0);
    let mut b = vec![0x7E, 0, 0, 0, 0, tag::LONG, 9];
    b.extend_from_slice(&nine);
    assert_eq!(loads(&b).unwrap(), Value::Int(i64::MAX));
    let mut big = vec![0x7E, 0, 0, 0, 0, tag::LONG, 10];
    big.extend_from_slice(&[0; 9]);
    big.push(1);
    assert!(matches!(loads(&big).unwrap(), Value::BigLong(_)));
}

#[test]
fn containers_roundtrip() {
    roundtrip(Value::tuple([]));
    roundtrip(Value::tuple([Value::Int(5)]));
    roundtrip(Value::tuple([Value::Int(5), Value::str("ab")]));
    roundtrip(Value::tuple((0..300).map(Value::Int)));
    roundtrip(Value::list([]));
    roundtrip(Value::list([Value::None]));
    roundtrip(Value::list([Value::None, Value::Float(2.5), Value::unicode("u")]));
    roundtrip(Value::dict([("a", Value::Int(1)), ("macho.CallRsp", Value::list([]))]));
    roundtrip(Value::global("util.KeyVal"));
    roundtrip(Value::instance("util.KeyVal", [("x", Value::Int(1))]));
    roundtrip(Value::reduce("blue.DBRowDescriptor", vec![Value::tuple([])], Some(Value::dict::<&str>([]))));
    roundtrip(Value::SubStream(dumps(&Value::str("inner"))));
    roundtrip(Value::Callback(Box::new(Value::str(dumps(&Value::tuple([Value::str("N=1:2"), Value::Int(5)]))))));
}

#[test]
fn dict_writes_value_before_key() {
    let b = dumps(&Value::dict([("k", Value::Int(7))]));
    assert_eq!(&b[5..], &[tag::DICT, 1, tag::INT8, 7, tag::STR_CHAR, b'k']);
}

#[test]
fn shared_references_version0() {
    // ~ count=1 | TUPLE2 | LIST1|SHARED [STR_CHAR 'x'] | REFERENCE 1 | map [1]
    let b = [0x7E, 1, 0, 0, 0, tag::TUPLE2, tag::LIST1 | tag::SHARED_FLAG, tag::STR_CHAR, b'x', tag::REFERENCE, 1, 1, 0, 0, 0];
    let x = Value::list([Value::str("x")]);
    assert_eq!(loads(&b).unwrap(), Value::tuple([x.clone(), x]));
}

#[test]
fn shared_slots_follow_the_map() {
    // Two shared objects; the map says the first-seen one occupies slot 2.
    let b = [
        0x7E, 2, 0, 0, 0, tag::TUPLE, 4, tag::LIST1 | tag::SHARED_FLAG, tag::INT8, 10, tag::LIST1 | tag::SHARED_FLAG,
        tag::INT8, 20, tag::REFERENCE, 1, tag::REFERENCE, 2, 2, 0, 0, 0, 1, 0, 0, 0,
    ];
    let a = Value::list([Value::Int(10)]);
    let c = Value::list([Value::Int(20)]);
    assert_eq!(loads(&b).unwrap(), Value::tuple([a.clone(), c.clone(), c, a]));
}

#[test]
fn rle_roundtrip_and_trailing_zero_trim() {
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![0; 20],
        vec![1, 2, 3],
        vec![1, 0, 0, 0, 5, 6, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
        (0..=255).collect(),
        vec![9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ];
    for c in cases {
        let p = rle_pack(&c);
        assert_eq!(rle_unpack(&p, c.len()).unwrap(), c, "packed {p:02x?}");
    }
    // All-zero data packs to nothing (trailing zero runs are trimmed).
    assert!(rle_pack(&[0; 16]).is_empty());
    // One literal byte: nibble (8-1)=7, then the byte.
    assert_eq!(rle_pack(&[0xAB]), vec![0x07, 0xAB]);
}

#[test]
fn dbrow_layout_matches_init_from_typed_list() {
    let d = DbRowDescriptor::new([
        ("itemID", DbType::I8),
        ("flag", DbType::UI1),
        ("singleton", DbType::Bool),
        ("quantity", DbType::I4),
        ("name", DbType::WStr),
        ("price", DbType::R8),
        ("active", DbType::Bool),
    ]);
    let l = d.layout();
    // 8-byte: itemID@0, price@8; 4-byte: quantity@16; 1-byte: flag@20; bools at bit 168,169;
    // null bits start at 170 for 7 real columns -> 177 bits -> 23 bytes.
    assert_eq!(
        l.slots,
        vec![
            Slot::Fixed { offset: 0, width: 8 },
            Slot::Fixed { offset: 20, width: 1 },
            Slot::Bit(168),
            Slot::Fixed { offset: 16, width: 4 },
            Slot::Object(0),
            Slot::Fixed { offset: 8, width: 8 },
            Slot::Bit(169),
        ]
    );
    assert_eq!((l.null_bits, l.data_len, l.n_objects), (170, 23, 1));
}

#[test]
fn decoded_dbrow_column_reader() {
    let descriptor = DbRowDescriptor::new([("itemID", DbType::I8), ("name", DbType::WStr), ("missing", DbType::I4)]);
    let row = descriptor.row(&[Value::Int(1_000_000_000_123), Value::unicode("item"), Value::None]);
    let decoded = loads(&dumps(&row)).unwrap();
    assert_eq!(descriptor.get(&decoded, "itemID"), Some(Value::Int(1_000_000_000_123)));
    assert_eq!(descriptor.get(&decoded, "name"), Some(Value::unicode("item")));
    assert_eq!(descriptor.get(&decoded, "missing"), Some(Value::None));
    assert_eq!(descriptor.get(&decoded, "unknown"), None);
}

#[test]
fn dbrow_roundtrip() {
    let d = DbRowDescriptor::new([("itemID", DbType::I8), ("name", DbType::WStr), ("singleton", DbType::Bool), ("q", DbType::I4)]);
    let row = d.row(&[Value::Int(1_000_000_000_123), Value::unicode("Rifter"), Value::Bool(true), Value::None]);
    roundtrip(row.clone());
    let Value::DbRow { descriptor, data, objects } = loads(&dumps(&row)).unwrap() else { panic!() };
    assert_eq!(DbRowDescriptor::from_value(&descriptor).unwrap(), d);
    assert_eq!(i64::from_le_bytes(data[0..8].try_into().unwrap()), 1_000_000_000_123);
    assert_eq!(objects, vec![Value::unicode("Rifter")]);
    let l = d.layout();
    let bit = |n: usize| data[n >> 3] & (1 << (n & 7)) != 0;
    assert!(bit(match l.slots[2] { Slot::Bit(b) => b, _ => unreachable!() }));
    assert!(bit(l.null_bits + 3), "q is NULL");
    assert!(!bit(l.null_bits));
}

#[test]
fn rejects_garbage() {
    assert!(matches!(loads(&[0x00]), Err(Error::BadSignature(0))));
    assert!(loads(&[0x7E, 0, 0, 0, 0, 63]).is_err());
    assert!(loads(&[0x7E, 0, 0, 0, 0, tag::STR_TABLE, 0]).is_err());
    assert!(loads(&[0x7E, 0, 0, 0, 0, tag::TUPLE, 5, tag::NONE]).is_err());
    assert!(loads(&[0x7E, 0, 0, 0, 0, tag::REFERENCE, 1]).is_err());
}
