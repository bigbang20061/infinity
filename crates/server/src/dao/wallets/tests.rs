use super::client_isk;
use crate::consts::{CLIENT_MAX_CORP_ISK, CLIENT_MAX_ISK};

#[test]
fn client_balance_is_clamped_without_changing_ordinary_amounts() {
    // At this magnitude adding or subtracting 1 rounds back to the cap; use adjacent doubles.
    let below = f64::from_bits(CLIENT_MAX_ISK.to_bits() - 1);
    let above = f64::from_bits(CLIENT_MAX_ISK.to_bits() + 1);
    assert_eq!(client_isk(below, CLIENT_MAX_ISK), below);
    assert_eq!(client_isk(CLIENT_MAX_ISK, CLIENT_MAX_ISK), CLIENT_MAX_ISK);
    assert_eq!(client_isk(above, CLIENT_MAX_ISK), CLIENT_MAX_ISK);
    assert_eq!(client_isk(1e34, CLIENT_MAX_ISK), CLIENT_MAX_ISK);
    assert_eq!(client_isk(123.45, CLIENT_MAX_ISK), 123.45);
    let below = f64::from_bits(CLIENT_MAX_CORP_ISK.to_bits() - 1);
    let above = f64::from_bits(CLIENT_MAX_CORP_ISK.to_bits() + 1);
    assert_eq!(client_isk(below, CLIENT_MAX_CORP_ISK), below);
    assert_eq!(client_isk(CLIENT_MAX_CORP_ISK, CLIENT_MAX_CORP_ISK), CLIENT_MAX_CORP_ISK);
    assert_eq!(client_isk(above, CLIENT_MAX_CORP_ISK), CLIENT_MAX_CORP_ISK);
    assert_eq!(client_isk(1e34, CLIENT_MAX_CORP_ISK), CLIENT_MAX_CORP_ISK);
    assert_eq!(client_isk(5e18, CLIENT_MAX_CORP_ISK), 5e18);
    assert_eq!(client_isk(5e18, CLIENT_MAX_ISK), CLIENT_MAX_ISK);
}
