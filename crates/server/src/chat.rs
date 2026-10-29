//! Minimal XMPP server for the client's chat (xmppchatclient / xmpp packages).
//!
//! Covers what the client speaks: stream setup, SASL PLAIN with tokens from
//! `chatAuthenticationService.GetAuthenticationToken`, resource bind, session,
//! presence, MUC rooms (`<channel>@conference.<host>/<charID>`) and direct
//! messages. Plain TCP: the launcher sets `chatserver_disable_tls=1` in prefs.ini.

use crate::consts::TYPE_CHARACTER;
use anyhow::{bail, Result};
use base64::Engine;
use quick_xml::events::Event;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// RFC 8259: JSON control characters below U+0020 require escaping.
const JSON_CONTROL_LIMIT: u32 = 0x20;
/// XEP-0045 status code identifying the recipient's own presence.
const MUC_SELF_PRESENCE: u16 = 110;

/// A parsed XML element (one stanza or a child of one).
#[derive(Debug, Clone, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }
    fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }
    /// Serialise, optionally overriding attributes.
    fn to_xml_with(&self, overrides: &[(&str, &str)]) -> String {
        let mut s = format!("<{}", self.name);
        for (k, v) in &self.attrs {
            if overrides.iter().any(|(o, _)| o == k) {
                continue;
            }
            s.push_str(&format!(" {k}='{}'", esc(v)));
        }
        for (k, v) in overrides {
            s.push_str(&format!(" {k}='{}'", esc(v)));
        }
        if self.children.is_empty() && self.text.is_empty() {
            s.push_str("/>");
            return s;
        }
        s.push('>');
        s.push_str(&esc(&self.text));
        for c in &self.children {
            s.push_str(&c.to_xml_with(&[]));
        }
        s.push_str(&format!("</{}>", self.name));
        s
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\'', "&apos;").replace('"', "&quot;")
}

type Tx = mpsc::UnboundedSender<String>;

struct Occupant {
    full_jid: String,
    tx: Tx,
}

/// Shared chat state: auth tokens, connected users, rooms.
#[derive(Default)]
pub struct Hub {
    pub host: String,
    tokens: Mutex<HashMap<String, String>>,
    users: Mutex<HashMap<String, (String, Tx)>>,
    rooms: Mutex<HashMap<String, HashMap<String, Occupant>>>,
    /// charID -> `<eve_user_data .../>` sent with presences and user data queries.
    profiles: Mutex<HashMap<String, String>>,
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < JSON_CONTROL_LIMIT => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Hub {
    pub fn new(host: &str) -> Arc<Hub> {
        Arc::new(Hub { host: host.to_string(), ..Default::default() })
    }

    /// Issue (or reuse) the chat password for a character.
    pub fn token_for(&self, char_id: i64) -> String {
        let mut t = self.tokens.lock().unwrap();
        t.entry(char_id.to_string()).or_insert_with(|| format!("{:016x}", crate::handshake::rand_i64())).clone()
    }

    fn check(&self, user: &str, password: &str) -> bool {
        self.tokens.lock().unwrap().get(user).is_some_and(|t| t == password)
    }

    /// What chat shows about a character (xmppchatsvc._GetPresenceUserData): affiliations,
    /// session role, and an eveowners row `[ownerID, ownerName, typeID, gender, ownerNameID]`.
    pub fn set_profile(&self, char_id: i64, name: &str, gender: i64, corp: i64, alliance: Option<i64>, faction: Option<i64>, role: i64) {
        let info = format!("[{char_id}, {}, {TYPE_CHARACTER}, {gender}, null]", json_str(name));
        let el = format!(
            "<eve_user_data corpid='{corp}' allianceid='{}' warfactionid='{}' typeid='{TYPE_CHARACTER}' role='{role}' info='{}'/>",
            alliance.unwrap_or(0),
            faction.unwrap_or(0),
            esc(&info)
        );
        self.profiles.lock().unwrap().insert(char_id.to_string(), el);
    }

    fn profile(&self, user: &str) -> String {
        self.profiles.lock().unwrap().get(user).cloned().unwrap_or_default()
    }

    fn conference(&self) -> String {
        format!("conference.{}", self.host)
    }

    /// Invite a character into a session channel (`local_<systemID>`...) after a
    /// session change; invites from `admin` are joined without asking (xmppchatsvc._HandleInvite).
    pub fn invite(&self, char_id: i64, channel: &str) {
        let users = self.users.lock().unwrap();
        let Some((jid, tx)) = users.get(&format!("{char_id}@{}", self.host)) else { return };
        let _ = tx.send(format!(
            "<message from='{channel}@{}' to='{jid}'><x xmlns='http://jabber.org/protocol/muc#user'><invite from='admin@{}'/></x></message>",
            self.conference(),
            self.host
        ));
    }

    /// Remove `full_jid` from every room it is in, telling the remaining occupants.
    fn leave_all(&self, full_jid: &str) {
        let mut rooms = self.rooms.lock().unwrap();
        for (room, occ) in rooms.iter_mut() {
            let gone: Vec<String> = occ.iter().filter(|(_, o)| o.full_jid == full_jid).map(|(n, _)| n.clone()).collect();
            for nick in gone {
                occ.remove(&nick);
                let p = format!("<presence from='{room}/{nick}' type='unavailable'><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='none' role='none'/></x></presence>");
                for o in occ.values() {
                    let _ = o.tx.send(p.clone());
                }
            }
        }
        rooms.retain(|_, o| !o.is_empty());
    }
}

pub async fn serve(bind: String, hub: Arc<Hub>) -> Result<()> {
    let listener = TcpListener::bind(&bind).await?;
    info!("chat (XMPP) listening on {bind}, domain {}", hub.host);
    loop {
        let (stream, peer) = listener.accept().await?;
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(stream, hub).await {
                debug!("chat connection {peer} ended: {e:#}");
            }
        });
    }
}

struct Conn {
    hub: Arc<Hub>,
    tx: Tx,
    user: Option<String>,
    full_jid: Option<String>,
}

async fn connection(stream: TcpStream, hub: Arc<Hub>) -> Result<()> {
    let (rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(s) = rx.recv().await {
            if wr.write_all(s.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut c = Conn { hub: hub.clone(), tx, user: None, full_jid: None };
    let result = read_loop(rd, &mut c).await;
    if let Some(jid) = &c.full_jid {
        hub.leave_all(jid);
        if let Some(user) = &c.user {
            hub.users.lock().unwrap().remove(&format!("{user}@{}", hub.host));
        }
        info!("chat: {jid} disconnected");
    }
    writer.abort();
    result
}

async fn read_loop(rd: tokio::net::tcp::OwnedReadHalf, c: &mut Conn) -> Result<()> {
    let mut reader = quick_xml::Reader::from_reader(BufReader::new(rd));
    reader.config_mut().check_end_names = false;
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    loop {
        buf.clear();
        let ev = reader.read_event_into_async(&mut buf).await?;
        match ev {
            Event::Eof => return Ok(()),
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"stream:stream" => {
                stack.clear();
                stream_opened(c);
            }
            Event::Start(e) => stack.push(element_from(&e)?),
            Event::Empty(e) => {
                let el = element_from(&e)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => stanza(c, el)?,
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.unescape()?);
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"stream:stream" {
                    let _ = c.tx.send("</stream:stream>".into());
                    return Ok(());
                }
                if let Some(el) = stack.pop() {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(el),
                        None => stanza(c, el)?,
                    }
                }
            }
            _ => {}
        }
    }
}

fn element_from(e: &quick_xml::events::BytesStart<'_>) -> Result<Element> {
    let mut el = Element { name: String::from_utf8_lossy(e.name().as_ref()).into_owned(), ..Default::default() };
    for a in e.attributes().flatten() {
        el.attrs.push((String::from_utf8_lossy(a.key.as_ref()).into_owned(), a.unescape_value()?.into_owned()));
    }
    Ok(el)
}

fn stream_opened(c: &mut Conn) {
    let host = &c.hub.host;
    let id = format!("{:x}", crate::handshake::rand_i64());
    let mut s = format!(
        "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' id='{id}' from='{host}' version='1.0' xml:lang='en'>"
    );
    if c.user.is_none() {
        s.push_str("<stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism></mechanisms></stream:features>");
    } else {
        s.push_str("<stream:features><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/><session xmlns='urn:ietf:params:xml:ns:xmpp-session'/></stream:features>");
    }
    let _ = c.tx.send(s);
}

fn stanza(c: &mut Conn, el: Element) -> Result<()> {
    match el.local() {
        "auth" => auth(c, &el),
        "iq" => iq(c, &el),
        "presence" => presence(c, &el),
        "message" => message(c, &el),
        other => {
            debug!("chat: ignoring <{other}>");
            Ok(())
        }
    }
}

fn auth(c: &mut Conn, el: &Element) -> Result<()> {
    let decoded = base64::engine::general_purpose::STANDARD.decode(el.text.trim()).unwrap_or_default();
    let parts: Vec<&[u8]> = decoded.split(|b| *b == 0).collect();
    let (user, pass) = match parts.as_slice() {
        [_, u, p] => (String::from_utf8_lossy(u).into_owned(), String::from_utf8_lossy(p).into_owned()),
        _ => (String::new(), String::new()),
    };
    if c.hub.check(&user, &pass) {
        c.user = Some(user);
        let _ = c.tx.send("<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>".into());
    } else {
        warn!("chat: auth failed for {user:?}");
        let _ = c.tx.send("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>".into());
    }
    Ok(())
}

fn iq(c: &mut Conn, el: &Element) -> Result<()> {
    let id = el.attr("id").unwrap_or("");
    let Some(user) = c.user.clone() else { bail!("iq before auth") };
    let host = c.hub.host.clone();
    if let Some(bind) = el.child("bind") {
        let resource = bind.child("resource").map(|r| r.text.trim().to_string()).filter(|r| !r.is_empty()).unwrap_or_else(|| "eve".into());
        let jid = format!("{user}@{host}/{resource}");
        c.hub.users.lock().unwrap().insert(format!("{user}@{host}"), (jid.clone(), c.tx.clone()));
        c.full_jid = Some(jid.clone());
        info!("chat: {jid} online");
        let _ = c.tx.send(format!(
            "<iq type='result' id='{}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>{}</jid></bind></iq>",
            esc(id),
            esc(&jid)
        ));
        return Ok(());
    }
    let to = el.attr("to").unwrap_or(&host);
    let from = c.full_jid.clone().unwrap_or_default();
    if let Some(q) = el.child("query").filter(|q| q.attr("xmlns") == Some("urn:xmpp:eve_user_data") && el.attr("type") == Some("get")) {
        let who = q.attr("jid").unwrap_or("").split('@').next().unwrap_or("");
        let _ = c.tx.send(format!("<iq type='result' id='{}' from='{}' to='{}'>{}</iq>", esc(id), esc(to), esc(&from), c.hub.profile(who)));
        return Ok(());
    }
    // session, ping, disco, private storage...: acknowledge with an empty result.
    debug!("chat: iq {} -> generic result: {}", el.attr("type").unwrap_or(""), el.to_xml_with(&[]));
    let _ = c.tx.send(format!("<iq type='result' id='{}' from='{}' to='{}'/>", esc(id), esc(to), esc(&from)));
    Ok(())
}

fn split_jid(jid: &str) -> (&str, Option<&str>) {
    match jid.split_once('/') {
        Some((bare, res)) => (bare, Some(res)),
        None => (jid, None),
    }
}

fn presence(c: &mut Conn, el: &Element) -> Result<()> {
    let Some(full) = c.full_jid.clone() else { return Ok(()) };
    let Some(to) = el.attr("to") else {
        // Initial/broadcast presence: echo it back as a server does. The client only
        // reports itself logged in (xmpp/xmpphandler.py) on its first non-iq stanza.
        if el.attr("type").is_none() {
            let _ = c.tx.send(format!("<presence from='{0}' to='{0}'/>", esc(&full)));
        }
        return Ok(());
    };
    let (room, nick) = split_jid(to);
    if !room.ends_with(&c.hub.conference()) {
        return Ok(());
    }
    let nick = nick.unwrap_or_else(|| c.user.as_deref().unwrap_or("unknown")).to_string();
    let id = el.attr("id").unwrap_or("").to_string();
    let item = |jid: &str| format!("<x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='member' role='participant' jid='{}'/>", esc(jid));

    if el.attr("type") == Some("unavailable") {
        let mut rooms = c.hub.rooms.lock().unwrap();
        if let Some(occ) = rooms.get_mut(room) {
            occ.remove(&nick);
            let p = format!("<presence from='{room}/{nick}' type='unavailable'>{}</x></presence>", item(&full));
            let _ = c.tx.send(format!("<presence id='{}' from='{room}/{nick}' to='{full}' type='unavailable'>{}<status code='{MUC_SELF_PRESENCE}'/></x></presence>", esc(&id), item(&full)));
            for o in occ.values() {
                let _ = o.tx.send(p.clone());
            }
        }
        return Ok(());
    }

    let mut rooms = c.hub.rooms.lock().unwrap();
    let occ = rooms.entry(room.to_string()).or_default();
    // Existing occupants to the newcomer, then the newcomer to everyone.
    for (n, o) in occ.iter() {
        if *n != nick {
            let _ = c.tx.send(format!("<presence from='{room}/{n}' to='{full}'>{}</x>{}</presence>", item(&o.full_jid), c.hub.profile(n)));
        }
    }
    let me = c.hub.profile(&nick);
    let announce = format!("<presence from='{room}/{nick}'>{}</x>{me}</presence>", item(&full));
    for (n, o) in occ.iter() {
        if *n != nick {
            let _ = o.tx.send(announce.clone());
        }
    }
    occ.insert(nick.clone(), Occupant { full_jid: full.clone(), tx: c.tx.clone() });
    let _ = c.tx.send(format!(
        "<presence id='{}' from='{room}/{nick}' to='{full}'>{}<status code='{MUC_SELF_PRESENCE}'/></x>{me}</presence>",
        esc(&id),
        item(&full)
    ));
    debug!("chat: {full} joined {room} ({} occupants)", occ.len());
    Ok(())
}

fn message(c: &mut Conn, el: &Element) -> Result<()> {
    let Some(full) = c.full_jid.clone() else { return Ok(()) };
    let Some(to) = el.attr("to") else { return Ok(()) };
    let (bare, _) = split_jid(to);
    if el.attr("type") == Some("groupchat") {
        let rooms = c.hub.rooms.lock().unwrap();
        let Some(occ) = rooms.get(bare) else {
            let _ = c.tx.send(format!(
                "<message from='{bare}' to='{full}' type='error'><error type='cancel'><item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></message>"
            ));
            return Ok(());
        };
        let nick = occ.iter().find(|(_, o)| o.full_jid == full).map(|(n, _)| n.clone()).unwrap_or_default();
        let from = format!("{bare}/{nick}");
        for o in occ.values() {
            let _ = o.tx.send(el.to_xml_with(&[("from", &from), ("to", &o.full_jid)]));
        }
        return Ok(());
    }
    // Direct message to a user.
    let users = c.hub.users.lock().unwrap();
    if let Some((jid, tx)) = users.get(bare) {
        let _ = tx.send(el.to_xml_with(&[("from", &full), ("to", jid)]));
    }
    Ok(())
}
