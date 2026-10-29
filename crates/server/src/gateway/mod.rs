//! Public gateway: the gRPC (HTTP/2, plaintext) service the client's native
//! `eve_public_grpc_client` talks to for newer features (wallet, map, corps,
//! goals, campaigns...). Point the client at it with
//! `EVE_PUBLIC_GATEWAY_ADDRESS=host:port` (publicGateway/grpc/connectionConfig.py).
//!
//! Services (eve_public/gateway.proto), all bidirectional streams:
//! * `Requests.Send`: `eve_public.Request` -> `eve_public.Response`, correlated by uuid
//! * `Notices.Consume`: server pushes `eve_public.Notice`
//! * `Events.Publish`: client telemetry, accepted and dropped
//!
//! Payloads are `google.protobuf.Any`; they are decoded with the client's own
//! descriptors (extracted by tools/extract_protos.py) through prost-reflect.

mod handlers;

use crate::db::Db;
use anyhow::{Context, Result};
use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, ReflectMessage};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// gRPC protocol: one compression byte followed by a four-byte message length.
const GRPC_HEADER_LEN: usize = 5;
/// RFC 4122: UUIDs contain 16 bytes.
const UUID_LEN: usize = 16;
/// Server limit for gateway request log excerpts, in bytes.
const LOG_EXCERPT_LEN: usize = 300;
/// Server limit for nesting when populating default gateway responses.
const DEFAULT_RESPONSE_DEPTH: u32 = 4;
/// Server limit for nesting when collecting request identifiers.
const IDENTIFIER_DEPTH: u32 = 2;
/// gRPC status code UNIMPLEMENTED, encoded as an ASCII trailer value.
const GRPC_UNIMPLEMENTED: &str = "12";

static DESCRIPTORS: &[u8] = include_bytes!("../../proto/eve_protos.desc");

pub fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        // The client ships a few conflicting duplicates (e.g. two files defining
        // eve.payment.customer.Identifier), so add files one at a time in dependency
        // order and skip the ones that cannot be added.
        let set = prost_types::FileDescriptorSet::decode(DESCRIPTORS).expect("client protobuf descriptor set");
        let mut pool = DescriptorPool::new();
        let mut pending = set.file;
        loop {
            let before = pending.len();
            let mut next = Vec::new();
            for fd in pending {
                let ready = fd.dependency.iter().all(|d| pool.get_file_by_name(d).is_some());
                if !ready {
                    next.push(fd);
                } else if let Err(e) = pool.add_file_descriptor_proto(fd.clone()) {
                    warn!("skipping proto {}: {e}", fd.name());
                }
            }
            pending = next;
            if pending.is_empty() || pending.len() == before {
                break;
            }
        }
        if !pending.is_empty() {
            warn!("{} proto files have unresolvable imports", pending.len());
        }
        pool
    })
}

// ---- envelope messages (eve_public/public.proto) ----

#[derive(Clone, PartialEq, Message)]
pub struct Identifier32 {
    #[prost(uint32, tag = "1")]
    pub sequential: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct UserIdentifier {
    #[prost(sint32, tag = "1")]
    pub sequential: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct ActiveIdentity {
    #[prost(message, optional, tag = "1")]
    pub character: Option<Identifier32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct AuthoritativeContext {
    #[prost(string, tag = "3")]
    pub tenant: String,
    #[prost(message, optional, tag = "5")]
    pub authenticated_user: Option<UserIdentifier>,
    #[prost(message, optional, tag = "7")]
    pub active_character: Option<Identifier32>,
    #[prost(message, optional, tag = "10")]
    pub identity: Option<ActiveIdentity>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Request {
    #[prost(message, optional, tag = "1")]
    pub issued: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub correlation_uuid: Vec<u8>,
    #[prost(message, optional, tag = "3")]
    pub payload: Option<prost_types::Any>,
    #[prost(string, tag = "4")]
    pub external_origin: String,
    #[prost(bytes = "vec", tag = "5")]
    pub application_instance_uuid: Vec<u8>,
    #[prost(message, optional, tag = "6")]
    pub authoritative_context: Option<AuthoritativeContext>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Response {
    #[prost(message, optional, tag = "1")]
    pub dispatched: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub correlation_uuid: Vec<u8>,
    #[prost(uint32, tag = "3")]
    pub status_code: u32,
    #[prost(string, tag = "4")]
    pub status_message: String,
    #[prost(message, optional, tag = "5")]
    pub payload: Option<prost_types::Any>,
    #[prost(string, tag = "6")]
    pub internal_origin: String,
    #[prost(bytes = "vec", tag = "7")]
    pub application_instance_uuid: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TargetGroup {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub application_instance_uuid: Option<Vec<u8>>,
    #[prost(uint32, optional, tag = "2")]
    pub solar_system: Option<u32>,
    #[prost(sint32, optional, tag = "3")]
    pub user: Option<i32>,
    #[prost(uint32, optional, tag = "4")]
    pub character: Option<u32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Notice {
    #[prost(message, optional, tag = "1")]
    pub dispatched: Option<prost_types::Timestamp>,
    #[prost(bytes = "vec", tag = "2")]
    pub uuid: Vec<u8>,
    #[prost(string, tag = "3")]
    pub internal_origin: String,
    #[prost(string, tag = "4")]
    pub tenant: String,
    #[prost(message, optional, tag = "5")]
    pub payload: Option<prost_types::Any>,
    #[prost(message, optional, tag = "6")]
    pub target_group: Option<TargetGroup>,
}

pub fn now_timestamp() -> prost_types::Timestamp {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    prost_types::Timestamp { seconds: d.as_secs() as i64, nanos: d.subsec_nanos() as i32 }
}

fn type_name(type_url: &str) -> &str {
    type_url.rsplit('/').next().unwrap_or(type_url)
}

pub fn pack(msg: &DynamicMessage) -> prost_types::Any {
    prost_types::Any { type_url: format!("type.googleapis.com/{}", msg.descriptor().full_name()), value: msg.encode_to_vec() }
}

pub fn message(name: &str) -> Option<MessageDescriptor> {
    pool().get_message_by_name(name)
}

pub(super) fn response_name(request: &str) -> Option<String> {
    request.strip_suffix("Request").map(|s| format!("{s}Response"))
}

/// Who sent a request.
pub struct Caller {
    pub user_id: Option<i64>,
    pub character_id: Option<i64>,
}

// ---- shared state: notice streams for server pushes ----

type NoticeTx = mpsc::UnboundedSender<Notice>;

#[derive(Default)]
pub struct Gateway {
    pub db: Option<Db>,
    /// Base URL of the content CDN, e.g. `http://127.0.0.1` (see crate::cdn).
    pub cdn_url: String,
    /// application instance uuid -> notice stream
    notice_streams: Mutex<HashMap<Vec<u8>, NoticeTx>>,
    /// application instance uuid -> (user, character) last seen on its requests
    identities: Mutex<HashMap<Vec<u8>, (Option<i64>, Option<i64>)>>,
}

impl Gateway {
    pub fn new(db: Db, cdn_url: String) -> Arc<Gateway> {
        Arc::new(Gateway { db: Some(db), cdn_url, ..Default::default() })
    }

    /// Push a notice to every client playing `character_id`.
    #[allow(dead_code)]
    pub fn notify_character(&self, character_id: i64, payload: &DynamicMessage) {
        let ids = self.identities.lock().unwrap();
        let streams = self.notice_streams.lock().unwrap();
        for (app, (_, ch)) in ids.iter() {
            if *ch == Some(character_id) {
                if let Some(tx) = streams.get(app) {
                    let _ = tx.send(Notice {
                        dispatched: Some(now_timestamp()),
                        uuid: crate::handshake::rand_i64().to_le_bytes().repeat(UUID_LEN / std::mem::size_of::<i64>()),
                        internal_origin: "infinity".into(),
                        tenant: String::new(),
                        payload: Some(pack(payload)),
                        target_group: Some(TargetGroup { character: Some(character_id as u32), ..Default::default() }),
                    });
                }
            }
        }
    }
}

// ---- gRPC over HTTP/2 ----

/// gRPC length-prefixed message framing.
fn grpc_frame(msg: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(GRPC_HEADER_LEN + msg.len());
    b.extend_from_slice(&[0]);
    b.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    b.extend_from_slice(msg);
    b.freeze()
}

/// Pull complete gRPC messages out of `buf`.
fn take_messages(buf: &mut BytesMut) -> Vec<Bytes> {
    let mut out = Vec::new();
    while buf.len() >= GRPC_HEADER_LEN {
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() < GRPC_HEADER_LEN + len {
            break;
        }
        buf.advance(GRPC_HEADER_LEN);
        out.push(buf.split_to(len).freeze());
    }
    out
}

fn ok_headers() -> http::Response<()> {
    http::Response::builder().status(http::StatusCode::OK).header("content-type", "application/grpc").body(()).unwrap()
}

fn ok_trailers() -> http::HeaderMap {
    let mut t = http::HeaderMap::new();
    t.insert("grpc-status", http::HeaderValue::from_static("0"));
    t
}

pub async fn serve(bind: String, gw: Arc<Gateway>) -> Result<()> {
    let listener = TcpListener::bind(&bind).await?;
    info!("public gateway (gRPC) listening on {bind}; {} message types loaded", pool().all_messages().count());
    loop {
        let (sock, peer) = listener.accept().await?;
        let gw = gw.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(sock, gw).await {
                debug!("gateway connection {peer} ended: {e:#}");
            }
        });
    }
}

async fn connection(sock: tokio::net::TcpStream, gw: Arc<Gateway>) -> Result<()> {
    let mut h2 = h2::server::handshake(sock).await?;
    while let Some(req) = h2.accept().await {
        let (req, respond) = req?;
        let gw = gw.clone();
        tokio::spawn(async move {
            let path = req.uri().path().to_string();
            if let Err(e) = stream(path.clone(), req.into_body(), respond, gw).await {
                debug!("gateway stream {path} ended: {e:#}");
            }
        });
    }
    Ok(())
}

async fn stream(
    path: String,
    mut body: h2::RecvStream,
    mut respond: h2::server::SendResponse<Bytes>,
    gw: Arc<Gateway>,
) -> Result<()> {
    let mut send = respond.send_response(ok_headers(), false)?;
    let mut buf = BytesMut::new();
    match path.as_str() {
        "/eve_public.gateway.Requests/Send" => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
                buf.extend_from_slice(&chunk);
                for msg in take_messages(&mut buf) {
                    let req = Request::decode(msg).context("decoding eve_public.Request")?;
                    let rsp = handle_request(&gw, req).await;
                    send.send_data(grpc_frame(&rsp.encode_to_vec()), false)?;
                }
            }
            send.send_trailers(ok_trailers())?;
        }
        "/eve_public.gateway.Notices/Consume" => {
            let (tx, mut rx) = mpsc::unbounded_channel::<Notice>();
            let mut app_uuid: Option<Vec<u8>> = None;
            loop {
                tokio::select! {
                    chunk = body.data() => {
                        let Some(chunk) = chunk else { break };
                        let chunk = chunk?;
                        body.flow_control().release_capacity(chunk.len()).ok();
                        buf.extend_from_slice(&chunk);
                        // The client's acks carry nothing we need; register the stream on first contact.
                        take_messages(&mut buf);
                        if app_uuid.is_none() {
                            let key = crate::handshake::rand_i64().to_le_bytes().to_vec();
                            gw.notice_streams.lock().unwrap().insert(key.clone(), tx.clone());
                            app_uuid = Some(key);
                        }
                    }
                    Some(n) = rx.recv() => {
                        send.send_data(grpc_frame(&n.encode_to_vec()), false)?;
                    }
                }
            }
            if let Some(k) = app_uuid {
                gw.notice_streams.lock().unwrap().remove(&k);
            }
            send.send_trailers(ok_trailers())?;
        }
        "/eve_public.gateway.Events/Publish" => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
            }
            send.send_trailers(ok_trailers())?;
        }
        // Ping on any service: Empty -> Empty.
        p if p.ends_with("/Ping") => {
            while let Some(chunk) = body.data().await {
                let chunk = chunk?;
                body.flow_control().release_capacity(chunk.len()).ok();
            }
            send.send_data(grpc_frame(&[]), false)?;
            send.send_trailers(ok_trailers())?;
        }
        other => {
            warn!("gateway: unknown gRPC method {other}");
            let mut t = http::HeaderMap::new();
            t.insert("grpc-status", http::HeaderValue::from_static(GRPC_UNIMPLEMENTED));
            send.send_trailers(t)?;
        }
    }
    Ok(())
}

async fn handle_request(gw: &Gateway, req: Request) -> Response {
    let ctx = req.authoritative_context.clone().unwrap_or_default();
    let caller = Caller {
        user_id: ctx.authenticated_user.map(|u| u.sequential as i64),
        character_id: ctx.identity.and_then(|i| i.character).or(ctx.active_character).map(|c| c.sequential as i64),
    };
    gw.identities.lock().unwrap().insert(req.application_instance_uuid.clone(), (caller.user_id, caller.character_id));
    let mut rsp = Response {
        dispatched: Some(now_timestamp()),
        correlation_uuid: req.correlation_uuid.clone(),
        status_code: http::StatusCode::OK.as_u16() as u32,
        status_message: String::new(),
        payload: None,
        internal_origin: "infinity".into(),
        application_instance_uuid: req.application_instance_uuid.clone(),
    };
    let Some(any) = req.payload else {
        rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
        rsp.status_message = "missing payload".into();
        return rsp;
    };
    let name = type_name(&any.type_url).to_string();
    let Some(desc) = message(&name) else {
        warn!("gateway: unknown request type {name}");
        rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
        rsp.status_message = format!("unknown type {name}");
        return rsp;
    };
    let payload = match DynamicMessage::decode(desc, any.value.as_slice()) {
        Ok(m) => m,
        Err(e) => {
            rsp.status_code = http::StatusCode::BAD_REQUEST.as_u16() as u32;
            rsp.status_message = format!("bad payload: {e}");
            return rsp;
        }
    };
    match handlers::handle(gw, &caller, &name, &payload).await {
        Ok(Some(out)) => {
            info!("gateway: {name} (char {:?}) -> {}", caller.character_id, out.descriptor().full_name());
            rsp.payload = Some(pack(&out));
        }
        Ok(None) if payload.descriptor().get_field_by_name("no_local_version_available").is_some() => {
            // CDN content checkpoints (content/client/flatbuffer/provider.py): no payload
            // means "nothing published", which the client treats as no content.
            debug!("gateway: no content for {name}");
        }
        Ok(None) => {
            // Unimplemented: answer with the matching `...Response` so the UI can proceed.
            let rsp_name = response_name(&name);
            match rsp_name.as_deref().and_then(message) {
                Some(d) => {
                    info!("gateway: default reply {} (char {:?}) for {name} {}", d.full_name(), caller.character_id, short_msg(&payload));
                    let mut m = DynamicMessage::new(d);
                    fill_defaults(&mut m, DEFAULT_RESPONSE_DEPTH, &known_ids(&payload, &caller));
                    rsp.payload = Some(pack(&m));
                }
                None => {
                    warn!("gateway: UNHANDLED {name} (no matching response type) {}", short_msg(&payload));
                    rsp.status_code = http::StatusCode::NOT_IMPLEMENTED.as_u16() as u32;
                    rsp.status_message = "not implemented".into();
                }
            }
        }
        Err(e) => {
            warn!("gateway: {name} failed: {e:#}");
            rsp.status_code = http::StatusCode::INTERNAL_SERVER_ERROR.as_u16() as u32;
            rsp.status_message = format!("{e:#}");
        }
    }
    rsp
}

/// Identifier-like sub-messages of a request (and the caller's character), by type,
/// so a default response can echo what it was asked about.
pub fn known_ids(req: &DynamicMessage, caller: &Caller) -> HashMap<String, DynamicMessage> {
    use prost_reflect::Value as PV;
    let mut ids = HashMap::new();
    if let (Some(ch), Some(d)) = (caller.character_id, message("eve_public.character.Identifier")) {
        let mut m = DynamicMessage::new(d);
        if m.try_set_field_by_name("sequential", PV::U32(ch as u32)).is_ok() {
            ids.insert(m.descriptor().full_name().to_string(), m);
        }
    }
    fn walk(m: &DynamicMessage, ids: &mut HashMap<String, DynamicMessage>, depth: u32) {
        for (f, v) in m.fields() {
            if let PV::Message(sub) = v {
                if f.is_list() || f.is_map() {
                    continue;
                }
                if sub.descriptor().full_name().ends_with(".Identifier") {
                    ids.insert(sub.descriptor().full_name().to_string(), sub.clone());
                } else if depth > 0 {
                    walk(sub, ids, depth - 1);
                }
            }
        }
    }
    walk(req, &mut ids, IDENTIFIER_DEPTH);
    ids
}

/// Make a default response safe to read:
/// * singular sub-messages are populated (echoing identifiers from `ids`),
/// * `uuid` byte fields get the all-zero UUID (the client's "none", e.g. skill plans),
/// * `eve_public.math.Fraction`s are 0/1 rather than 0/0,
/// * a oneof offering a "nothing" flag (`no_title`, `no_equipped`, `not_completed`,
///   `none`...) has it set, which is how these APIs say "absent".
pub fn fill_defaults(m: &mut DynamicMessage, depth: u32, ids: &HashMap<String, DynamicMessage>) {
    use prost_reflect::{Kind, Value as PV};
    if depth == 0 {
        return;
    }
    let desc = m.descriptor();
    if desc.full_name() == "eve_public.math.Fraction" {
        let _ = m.try_set_field_by_name("denominator", PV::U64(1));
        return;
    }
    for o in desc.oneofs() {
        if o.fields().any(|f| m.has_field(&f)) {
            continue;
        }
        let nothing = o.fields().find(|f| {
            let n = f.name();
            matches!(f.kind(), Kind::Bool) && (n.starts_with("no_") || n.starts_with("not_") || n == "none")
        });
        if let Some(f) = nothing {
            m.set_field(&f, PV::Bool(true));
        }
    }
    for f in desc.fields() {
        if f.is_list() || f.is_map() || f.containing_oneof().is_some() {
            continue;
        }
        match f.kind() {
            Kind::Bytes if f.name() == "uuid" => m.set_field(&f, PV::Bytes(Bytes::from_static(&[0u8; UUID_LEN]))),
            Kind::Message(md) if !md.full_name().starts_with("google.protobuf.") => {
                if let Some(id) = ids.get(md.full_name()) {
                    m.set_field(&f, PV::Message(id.clone()));
                    continue;
                }
                let mut sub = DynamicMessage::new(md);
                fill_defaults(&mut sub, depth - 1, ids);
                m.set_field(&f, PV::Message(sub));
            }
            _ => {}
        }
    }
}

fn short_msg(m: &DynamicMessage) -> String {
    let s = format!("{m}");
    if s.len() > LOG_EXCERPT_LEN {
        format!("{}...", &s[..s.char_indices().take_while(|(i, _)| *i < LOG_EXCERPT_LEN).last().map(|(i, _)| i).unwrap_or(0)])
    } else {
        s
    }
}


#[cfg(test)]
mod tests {
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
}