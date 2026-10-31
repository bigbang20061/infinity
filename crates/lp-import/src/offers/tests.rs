//! Focused validation and mock-HTTP checks for the operator importer.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn payload() -> Value {
    json!([{ "offer_id": 7, "type_id": 10, "quantity": 2, "lp_cost": 3,
        "isk_cost": 4, "ak_cost": 0, "required_items": [
            {"type_id": 12, "quantity": 5}, {"type_id": 11, "quantity": 6}
        ] }])
}

fn types() -> HashSet<i32> { [10, 11, 12].into_iter().collect() }

#[test]
fn validation_preserves_order_and_is_repeatable() {
    let bytes = serde_json::to_vec(&payload()).unwrap();
    for _ in 0..2 {
        let offers = parse(&bytes, &types()).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].required_items, json!([{"type_id":12,"quantity":5},{"type_id":11,"quantity":6}]));
    }
}
#[test]
fn optional_ak_cost_defaults_only_when_absent() {
    let mut v = payload();
    v[0].as_object_mut().unwrap().remove("ak_cost");
    assert_eq!(parse(&serde_json::to_vec(&v).unwrap(), &types()).unwrap()[0].ak_cost, 0);
    for value in [json!(null), json!("0"), json!(1.5), json!(-1)] {
        v[0]["ak_cost"] = value;
        assert!(parse(&serde_json::to_vec(&v).unwrap(), &types()).is_err());
    }
}


#[test]
fn rejects_bad_fields_and_duplicate_offers() {
    for (field, value) in [("offer_id", json!(0)), ("type_id", json!(99)), ("quantity", json!(0)),
        ("quantity", json!(2147483648_i64)), ("lp_cost", json!(-1)), ("isk_cost", json!(-1)),
        ("ak_cost", json!(-1)), ("required_items", json!(null))] {
        let mut v = payload();
        v[0][field] = value;
        assert!(parse(&serde_json::to_vec(&v).unwrap(), &types()).is_err(), "{field}");
    }
    let mut v = payload();
    v[0]["required_items"][0]["quantity"] = json!(0);
    assert!(parse(&serde_json::to_vec(&v).unwrap(), &types()).is_err());
    let mut v = payload();
    v[0]["required_items"][0]["type_id"] = json!(99);
    assert!(parse(&serde_json::to_vec(&v).unwrap(), &types()).is_err());
    let mut v = payload();
    let duplicate = v[0].clone();
    v.as_array_mut().unwrap().push(duplicate);
    assert!(parse(&serde_json::to_vec(&v).unwrap(), &types()).is_err());
}

#[tokio::test]
async fn fetch_checks_status_and_parses_mock_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for status in ["200 OK", "500 Internal Server Error"] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /loyalty/stores/42/offers/ "));
            let body = payload().to_string();
            let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");
    let limit = payload().to_string().len();
    assert_eq!(fetch(&client, &base, 42, &types(), limit).await.unwrap().len(), 1);
    assert!(fetch(&client, &base, 42, &types(), limit).await.is_err());
    server.await.unwrap();
}

#[tokio::test]
async fn fetch_rejects_oversized_declared_and_chunked_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for response in [
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n1234\r\n5\r\n56789\r\n0\r\n\r\n",
        ] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            stream.read(&mut buf).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");
    for _ in 0..2 {
        let error = fetch(&client, &base, 42, &types(), 8).await.err().unwrap();
        assert!(error.to_string().contains("exceeds 8 bytes"), "{error:#}");
    }
    server.await.unwrap();
    assert!(fetch(&client, &base, 42, &types(), 0).await.is_err());
}
