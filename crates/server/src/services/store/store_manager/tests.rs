use super::*;

#[test]
fn service_prices_use_only_positive_retail_prices() {
    assert_eq!(service_price(None, 100.0, 50), 50);
    assert_eq!(service_price(Some(0.0), 100.0, 50), 50);
    assert_eq!(service_price(Some(-1.0), 100.0, 50), 50);
    assert_eq!(service_price(Some(240.0), 100.0, 50), 2);
    assert_eq!(service_price(Some(1.0), 100.0, 50), 1);
}

#[test]
fn retail_price_filter_matches_postgres_float_ordering() {
    use crate::dao::store::catalogue::positive_price;
    assert!(positive_price(f64::NAN));
    assert!(positive_price(f64::INFINITY));
    assert!(positive_price(1.0));
    assert!(!positive_price(0.0));
    assert!(!positive_price(-1.0));
    assert!(!positive_price(f64::NEG_INFINITY));
    assert_eq!(service_price(Some(f64::NAN), 100.0, 50), 1);
}
