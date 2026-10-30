//! GM slash commands (the `slash` service). Chat sends `/command args` here when the
//! session has a slash role (ROLE_GML | ROLE_LEGIONEER, chatSlashCmds); the client's
//! own svc.slash handles its local commands first and forwards the rest, and the GM
//! context menu sends `/tr me <itemID>`.
//!
//! `SlashCmd('/')` answers with the command list in a UserError's `reason`
//! (svc_slash.FetchRemoteCommandList evals what follows ': ').

use super::{db_err, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{FLAG_CARGO, FLAG_HANGAR, GM_ROLES};
use crate::messages;
use marshal::Value;
use tracing::info;

/// Server design: clearance in metres when teleporting to an item.
const TELEPORT_CLEARANCE: f64 = 5000.0;

struct TypeRow { type_id: i32, name: String }
impl TypeRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { type_id: r.get("type_id"), name: r.get("name") } } }
struct Celestial { x: f64, y: f64, z: f64, radius: f64 }
impl Celestial { fn from_row(r: &tokio_postgres::Row) -> Self { Self { x: r.get("x"), y: r.get("y"), z: r.get("z"), radius: r.get("radius") } } }
struct SystemCelestial { solar_system_id: i64, celestial: Celestial }
impl SystemCelestial { fn from_row(r: &tokio_postgres::Row) -> Self { Self { solar_system_id: r.get("solar_system_id"), celestial: Celestial::from_row(r) } } }

const COMMANDS: [(&str, &str); 9] = [
    ("tr", messages::GM_HELP_TELEPORT),
    ("wh", messages::GM_HELP_WORMHOLE),
    ("scanall", messages::GM_HELP_SCAN_ALL),
    ("sites", messages::GM_HELP_SITES),
    ("create", messages::GM_HELP_CREATE),
    ("createitem", messages::GM_HELP_CREATE_ITEM),
    ("isk", messages::GM_HELP_ISK),
    ("heal", messages::GM_HELP_HEAL),
    ("help", messages::GM_HELP_HELP),
];

fn slash_error(text: String) -> CallError {
    CallError::User { msg: messages::MSG_SLASH_ERROR.into(), dict: vec![("reason".into(), Value::unicode(text))] }
}

fn help() -> String {
    COMMANDS.iter().map(|(_, usage)| *usage).collect::<Vec<_>>().join("\n")
}

/// A type by ID or (partial) name.
async fn resolve_type(c: &Client, arg: &str) -> Result<(i32, String), CallError> {
    let row = match arg.parse::<i32>() {
        Ok(id) => c.db.query_opt("SELECT type_id, name FROM sde.inv_types WHERE type_id = $1", &[&id]).await,
        Err(_) => {
            c.db
                .query_opt(
                    "SELECT type_id, name FROM sde.inv_types WHERE published AND name ILIKE '%' || $1 || '%'
                     ORDER BY lower(name) = lower($1) DESC, length(name) LIMIT 1",
                    &[&arg],
                )
                .await
        }
    }
    .map_err(db_err)?;
    row.map(|r| { let r = TypeRow::from_row(&r); (r.type_id, r.name) }).ok_or_else(|| slash_error(messages::gm_unknown_type(arg)))
}

/// /tr me <destination>
async fn tr(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let dest = match args {
        ["me", dest, ..] => *dest,
        [dest] if *dest != "me" => *dest,
        _ => return Err(slash_error(messages::GM_TELEPORT_USAGE.into())),
    };
    let Ok(id) = dest.parse::<i64>() else { return Err(slash_error(messages::gm_invalid_id(dest))) };
    if c.session.int("stationid").is_some() {
        return Err(slash_error(messages::GM_UNDOCK_FIRST.into()));
    }
    let here = c.session.int("solarsystemid").unwrap_or(0);
    // Something in this system's ballpark.
    if let Some((ball_pos, radius)) = c.world.with(here, move |s| s.ball(id).map(|b| (b.pos, b.radius))).await.flatten() {
        let pos = [ball_pos[0] + radius as f64 + TELEPORT_CLEARANCE, ball_pos[1], ball_pos[2]];
        super::ship::teleport(c, here, pos).await?;
        return Ok(messages::gm_teleported(id));
    }
    // A solar system: off one of its planets (or its sun).
    if (30_000_000..33_000_000).contains(&id) {
        let p = c
            .db
            .query_opt(
                "SELECT x, y, z, COALESCE(radius, 10000)::float8 AS radius FROM sde.map_celestials WHERE solar_system_id = $1
                 ORDER BY group_id = 7 DESC, random() LIMIT 1",
                &[&(id as i32)],
            )
            .await
            .map_err(db_err)?
            .ok_or_else(|| slash_error(messages::gm_unknown_system(id)))?;
        let p = Celestial::from_row(&p);
        let pos = [p.x + p.radius + 100_000.0, p.y, p.z];
        super::ship::teleport(c, id, pos).await?;
        return Ok(messages::gm_teleported_to_system(id));
    }
    // A station, planet, moon, gate...
    if let Some(p) = c
        .db
        .query_opt("SELECT solar_system_id::bigint, x, y, z, COALESCE(radius, 5000)::float8 AS radius FROM sde.map_celestials WHERE item_id = $1", &[&id])
        .await
        .map_err(db_err)?
    {
        let p = SystemCelestial::from_row(&p);
        let pos = [p.celestial.x + p.celestial.radius + TELEPORT_CLEARANCE, p.celestial.y, p.celestial.z];
        super::ship::teleport(c, p.solar_system_id, pos).await?;
        return Ok(messages::gm_teleported(id));
    }
    // A character in space, or an item floating somewhere.
    let at = crate::dao::items::social::position(&c.db, id).await.map_err(db_err)?;
    if let Some(r) = at {
        let system = r.location_id;
        let pos = [r.x + TELEPORT_CLEARANCE, r.y, r.z];
        let live = c.world.with(system, move |s| s.pilot_ball(id).or_else(|| s.ball(id)).map(|b| b.pos)).await.flatten();
        let pos = live.map(|p| [p[0] + TELEPORT_CLEARANCE, p[1], p[2]]).unwrap_or(pos);
        super::ship::teleport(c, system, pos).await?;
        return Ok(messages::gm_teleported_in_system(id, system));
    }
    Err(slash_error(messages::gm_target_not_found(id)))
}

async fn create(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let Some(what) = args.first() else { return Err(slash_error(messages::GM_CREATE_USAGE.into())) };
    let qty: i32 = args.get(1).and_then(|q| q.parse().ok()).unwrap_or(1).clamp(1, 1_000_000_000);
    let (type_id, name) = resolve_type(c, what).await?;
    let char_id = c.session.int("charid").unwrap_or(0);
    let (location, flag) = match (c.session.int("stationid"), c.session.int("shipid")) {
        (Some(station), _) => (station, FLAG_HANGAR),
        (None, Some(ship)) => (ship, FLAG_CARGO),
        _ => return Err(slash_error(messages::GM_NOWHERE_TO_PUT_ITEM.into())),
    };
    let mut outbox = crate::db::outbox::Outbox::default();
    super::items::deposit(&c.db, &mut outbox, char_id, location, flag, type_id, qty, 0).await?;
    crate::db::outbox::dispatch_client((), outbox, c)?;
    Ok(messages::gm_created_item(qty, &name, type_id))
}

async fn isk(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let amount: f64 = args.first().and_then(|a| a.replace(',', "").parse().ok()).ok_or_else(|| slash_error(messages::GM_ISK_USAGE.into()))?;
    let char_id = c.session.int("charid").unwrap_or(0);
    let balance = crate::dao::wallets::social::add(&c.db, char_id, amount).await.map_err(db_err)?;
    c.notify("OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
    Ok(messages::gm_balance(balance))
}

async fn heal(c: &mut Client) -> Result<String, CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok(messages::GM_DOCKED_SHIP_REPAIRS.into()) };
    super::combat::repair(ship);
    c.world.set_health(system, ship, [1.0; 3]);
    super::ship::send_state(c).await?;
    Ok(messages::GM_REPAIRED.into())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "slash" || call.method != "SlashCmd" {
        return None;
    }
    if c.login.role & GM_ROLES == 0 {
        return Some(Err(slash_error(messages::GM_ROLE_REQUIRED.into())));
    }
    let line = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let line = line.trim().trim_start_matches('/');
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or("").to_lowercase();
    let args: Vec<&str> = words.collect();
    if command.is_empty() {
        let names: Vec<String> = COMMANDS.iter().map(|(n, _)| messages::gm_command_name(n)).collect();
        return Some(Err(slash_error(messages::gm_command_list(&names.join(", ")))));
    }
    info!(user = c.login.user_id, "slash: /{line}");
    let result = match command.as_str() {
        "tr" => tr(c, &args).await,
        "wh" | "wormhole" => super::exploration::gm_wormhole(c).await,
        "scanall" => super::exploration::gm_scan_all(c).await,
        "sites" => super::exploration::gm_sites(c).await,
        "create" | "createitem" => create(c, &args).await,
        "isk" => isk(c, &args).await,
        "heal" => heal(c).await,
        "help" => Ok(help()),
        other => Err(slash_error(messages::gm_unknown_command(other, &help()))),
    };
    Some(result.map(Value::unicode))
}
