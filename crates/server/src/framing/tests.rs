use super::{inflate, read_frame, FLAG_ZLIB};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

#[tokio::test]
async fn inflates_zlib_payload_and_frame() {
    let payload = b"compressed packet payload";
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).unwrap();
    let compressed = encoder.finish().unwrap();

    assert_eq!(inflate(&compressed).unwrap(), payload);

    let header = FLAG_ZLIB | compressed.len() as u32;
    let mut frame = header.to_le_bytes().to_vec();
    frame.extend_from_slice(&compressed);
    let decoded = read_frame(&mut frame.as_slice()).await.unwrap().unwrap();
    assert_eq!(decoded.payload, payload);
}

#[test]
fn rejects_malformed_zlib() {
    assert!(inflate(&[0xff, 0xff, 0xff, 0xff]).is_err());
}
