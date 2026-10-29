//! Remote services the client calls (`sm.RemoteSvc(name).Method(...)`).

mod agency;
mod alliances;
mod bills;
mod bookmarks;
mod calendar;
mod character;
mod clones;
mod combat;
mod charsel;
mod config;
mod contracts;
mod corps;
mod corpwallet;
mod crimewatch;
mod dogma;
mod exploration;
mod fit;
mod fleets;
mod fitting;
mod hacking;
mod industry;
mod insurance;
mod inventory;
mod mail;
mod market;
mod npe;
mod offices;
mod orgs;
mod mining;
mod missions;
mod ship;
mod skills;
mod standings;
mod slash;
mod station;
mod store;
mod universe;

pub use contracts::settle_auctions;
pub use fleets::logged_off as fleet_logged_off;
pub use market::expire_orders as expire_market_orders;
pub use bills::run_billing;
pub use exploration::unbury_sites;

use crate::client::{filetime_now, short, Client};
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::warn;

/// The pilot's ship was destroyed: eject (raised by combat through the pilot's notify channel).
pub async fn ship_destroyed(c: &mut Client, args: &[Value]) {
    let n = |i: usize| args.get(i).and_then(|v| v.as_float()).unwrap_or(0.0);
    let ship = args.first().and_then(|v| v.as_int()).unwrap_or(0);
    if let Err(CallError::Internal(e)) = ship::lose_ship(c, ship, [n(1), n(2), n(3)]).await {
        warn!(user = c.login.user_id, "ejecting from {ship} failed: {e:#}");
    }
}

pub struct Call<'a> {
    pub service: &'a str,
    pub method: &'a str,
    pub args: &'a [Value],
    pub kwargs: &'a Value,
    /// Call on a bound object (moniker) rather than a service.
    pub bound: bool,
    /// The moniker's bindParams for bound calls.
    pub bind_params: Option<&'a Value>,
}

pub enum CallError {
    /// Shown to the player via `eveexceptions.UserError(msg, dict)`.
    #[allow(dead_code)]
    User { msg: String, dict: Vec<(String, Value)> },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CallError {
    fn from(e: anyhow::Error) -> Self {
        CallError::Internal(e)
    }
}

pub type CallResult = Result<Value, CallError>;

/// `utillib.KeyVal(**kw)`: attribute-access record.
pub fn keyval<K: AsRef<[u8]>>(fields: impl IntoIterator<Item = (K, Value)>) -> Value {
    Value::instance("utillib.KeyVal", fields)
}

/// `eve.common.script.sys.rowset.Rowset(header, lines)` (old-style instance, plain __dict__).
pub fn rowset(header: &[&str], lines: Vec<Vec<Value>>) -> Value {
    Value::instance(
        "eve.common.script.sys.rowset.Rowset",
        [
            ("header", Value::list(header.iter().map(|h| Value::str(*h)))),
            ("lines", Value::List(lines.into_iter().map(Value::List).collect())),
            ("RowClass", Value::global("carbon.common.script.sys.row.Row")),
        ],
    )
}

/// `dbutil.CRowset(header, rows)`: a list of `blue.DBRow`s sharing `header`.
pub fn crowset(header: &DbRowDescriptor, rows: Vec<Value>) -> Value {
    Value::reduce("carbon.common.script.sys.crowset.CRowset", vec![header.to_value(), Value::List(rows)], None)
}

/// Columns of an inventory row (invbroker.GetItemDescriptor).
pub fn item_descriptor() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("itemID", DbType::I8),
        ("typeID", DbType::I4),
        ("ownerID", DbType::I4),
        ("locationID", DbType::I8),
        ("flagID", DbType::I2),
        ("quantity", DbType::I4),
        ("groupID", DbType::I2),
        ("categoryID", DbType::I1),
        ("customInfo", DbType::Str),
    ])
    .with_virtual([("stacksize", "eve.common.script.sys.eveCfg.StackSize"), ("singleton", "eve.common.script.sys.eveCfg.Singleton")])
}

const FAR_FUTURE: i64 = 157_766_976_000_000_000; // 2100-12-31 as FILETIME

/// `MachoBindObject(bindParams, call)` -> `(boundObject, result of call or None)`.
async fn bind_object(c: &mut Client, call: &Call<'_>) -> CallResult {
    let params = call.args.first().cloned().unwrap_or(Value::None);
    let object = c.bind(call.service, params.clone());
    let mut ret = Value::None;
    if let Some([method, args, kwargs]) = call.args.get(1).and_then(|v| v.items()) {
        let method = method.as_text().map(|m| m.into_owned()).unwrap_or_default();
        let args = args.items().map(|a| a.to_vec()).unwrap_or_default();
        let inner = Call { service: call.service, method: &method, args: &args, kwargs, bound: true, bind_params: Some(&params) };
        ret = Box::pin(dispatch(c, &inner)).await?;
    }
    // Binding the remote ballpark is the client's signal that it is ready for destiny state.
    if call.service == "beyonce" {
        ship::send_state(c).await?;
    }
    Ok(Value::tuple([object, ret]))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    // What the retail server answered for this build, where the client cached it.
    if let Some(v) = crate::retail::cached_call(call.service, call.method, call.args) {
        return Ok(v);
    }
    let s = Value::str;
    match (call.service, call.method) {
        // Monikers: every bindable service lives on this node.
        (_, "MachoResolveObject") => Ok(Value::Int(c.cfg.node_id)),
        (_, "MachoBindObject") => bind_object(c, call).await,

        // Which node each service lives on; empty = "any node", i.e. us.
        ("machoNet", "GetServiceInfo") => Ok(Value::Dict(vec![])),
        ("machoNet", "GetTime") => Ok(Value::Int(filetime_now())),
        ("machoNet", "GetGlobalConfig") => Ok(c.cfg.global_config()),
        ("chatAuthenticationService", "GetAuthenticationToken") => {
            let char_id = c.session.int("charid").unwrap_or(0);
            Ok(Value::str(c.chat.token_for(char_id)))
        }
        ("XmppChatMgr", "Hostname") => Ok(Value::str(&c.cfg.chat_domain)),
        ("XmppChatMgr", "GetDeprecatedPrefsFallback") => Ok(Value::str(&c.cfg.public_host)),
        // Session channels the client joins after chat login (xmppchatsvc._JoinSystemChannels);
        // an empty list makes it retry every 5 seconds.
        ("XmppChatMgr", "ResyncSystemChannelAccess") => {
            let channels = [("local", "solarsystemid2"), ("corp", "corpid"), ("alliance", "allianceid"), ("faction", "warfactionid"), ("fleet", "fleetid")]
                .into_iter()
                .filter_map(|(prefix, key)| c.session.int(key).map(|id| Value::str(format!("{prefix}_{id}"))));
            Ok(Value::list(channels))
        }

        (inventory::INVENTORY, _) => inventory::inventory(c, call).await,

        ("config", _) => config::dispatch(c, call).await,

        ("air_npe", "is_air_npe_enabled") => Ok(Value::Bool(false)),
        // (errorID, logMode): a known error ID means "don't send me the trace".
        ("alert", "BeanCount") => Ok(Value::tuple([Value::Int(1), s("B")])), // LOGGING_BATCHED
        ("alert", "SendClientStackTraceAlert") => Ok(Value::tuple([Value::Int(1), s("B")])),
        // (changes {errorHash: logMode}, flooded)
        ("alert", "GetLogModeForError") => Ok(Value::tuple([Value::Dict(vec![]), Value::list([])])),

        ("subscriptionMgr", "GetCloneGrade") => Ok(Value::Int(1)), // CLONE_STATE_OMEGA
        ("subscriptionMgr", "GetSubscriptionTime") => Ok(Value::Int(FAR_FUTURE)),

        // NetEase account/payment gateway; endpoints are never contacted here.
        ("kiringMgr", "GetKiringConfiguration") => Ok(Value::dict([
            ("mode", s("release")),
            ("client_id", s("infinity-emu")),
            (
                "endpoints",
                Value::dict([("mpay", s("http://127.0.0.1/")), ("billing", s("http://127.0.0.1/")), ("redirect_uri", s("http://127.0.0.1/"))]),
            ),
            ("channels", Value::dict([("login", s("emu")), ("pay", s("emu")), ("app", s("emu"))])),
        ])),
        ("kiringMgr", "IsMonochromeStyleEnabled") => Ok(Value::Bool(false)),
        // kiring.common.const.ANTI_ADDICTION_PASS_CODE
        ("kiringMgr", "GetFaceRecognitionCode") => Ok(Value::tuple([Value::Int(201), s("")])),
        ("kiringMgr", "GetAntiAddictionCode") => Ok(Value::Int(201)),
        ("kiringMgr", "IsZhStoreEnabled") => Ok(Value::Bool(false)),

        ("pingService", "Ping") => Ok(Value::None),
        // {solarSystemID: slotKey} of alternative load systems; empty = load where the character is.
        ("populationCap", "GetCharacterLoadSlot") => Ok(Value::Dict(vec![])),
        ("eventLog", _) => Ok(Value::None),

        ("charUnboundMgr", _) => charsel::dispatch(c, call).await,
        ("userSvc", "UserLogOffCharacter") => charsel::log_off(c).await,
        ("dogmaIM", _) => dogma::dispatch(c, call).await,
        // {raceID: {solarSystemID: population}}; empty = no starter system is capped.
        ("populationCap", "GetCharacterCreationSystemPopulations") => Ok(Value::Dict(vec![])),

        // Login reward campaigns: none running.
        ("loginCampaignManager", "get_client_campaign_state") => Ok(Value::None),

        _ => {
            if call.service == "invbroker" {
                if let Some(r) = inventory::broker(c, call).await {
                    return r;
                }
            }
            if let Some(r) = crimewatch::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = calendar::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = bookmarks::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = clones::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = insurance::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fleets::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = orgs::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = corpwallet::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = offices::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = bills::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = alliances::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = corps::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = standings::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = ship::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = fitting::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = skills::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = character::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = store::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = market::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = contracts::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = industry::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = missions::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = slash::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = hacking::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = mining::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = exploration::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = agency::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = npe::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = mail::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = universe::dispatch(c, call).await {
                return r;
            }
            if let Some(r) = station::dispatch(c, call).await {
                return r;
            }
            warn!(
                user = c.login.user_id,
                "UNHANDLED {}{}::{} args={} kwargs={}",
                if call.bound { "bound " } else { "" },
                call.service,
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
