use super::*;

#[test]
fn crc_hqx_matches_python() {
    // binascii.crc_hqx(b'123456789', 0) == 0x31C3
    assert_eq!(crc_hqx(b"123456789", 0), 0x31C3);
    assert_eq!(crc_hqx(b"", 0), 0);
}
