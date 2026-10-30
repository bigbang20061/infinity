use super::*;

#[test]
fn response_name_requires_request_suffix() {
    assert_eq!(response_name("eve_public.ExampleRequest").as_deref(), Some("eve_public.ExampleResponse"));
    assert_eq!(response_name("eve_public.Example"), None);
}

#[test]
fn descriptor_pool_resolves() {
    assert!(message("eve_public.Request").is_some());
    assert!(message("google.protobuf.Any").is_some());
    let svc = pool().get_service_by_name("eve_public.gateway.Requests").expect("Requests service");
    assert!(svc.methods().any(|m| m.name() == "Send"));
}

#[test]
fn grpc_framing_roundtrip() {
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&grpc_frame(b"abc"));
    buf.extend_from_slice(&grpc_frame(b""));
    buf.extend_from_slice(&grpc_frame(b"xy")[..3]);
    let msgs = take_messages(&mut buf);
    assert_eq!(msgs, vec![Bytes::from_static(b"abc"), Bytes::new()]);
    assert_eq!(buf.len(), 3);
}
