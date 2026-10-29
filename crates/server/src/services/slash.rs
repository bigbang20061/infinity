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
use marshal::Value;
use tracing::info;

const COMMANDS: [(&str, &str); 9] = [
    ("tr", "/tr me <itemID|characterID|solarSystemID|stationID|celestialID> — teleport"),
    ("wh", "/wh — teleport next to a wormhole (scanned down for you)"),
    ("scanall", "/scanall — scan every signature in this system to 100%"),
    ("sites", "/sites — list this system's anomalies and signatures"),
    ("create", "/create <typeID|name> [qty] — into your hangar (docked) or cargo"),
    ("createitem", "/createitem <typeID|name> [qty] — same as /create"),
    ("isk", "/isk <amount> — add ISK to your wallet"),
    ("heal", "/heal — repair your ship in space"),
    ("help", "/help — this list"),
];

fn slash_error(text: String) -> CallError {
    CallError::User { msg: "SlashError".into(), dict: vec![("reason".into(), Value::unicode(text))] }
}

fn help() -> String {
    COMMANDS.iter().map(|(_, usage)| *usage).collect::<Vec<_>>().join("\n")
}

/// A type by ID or (partial) name.
async fn resolve_type(c: &Client, arg: &str) -> Result<(i32, String), CallError> {
    let row = match arg.parse::<i32>() {
        Ok(id) => c.db.client.query_opt("SELECT type_id, name FROM sde.inv_types WHERE type_id = $1", &[&id]).await,
        Err(_) => {
            c.db.client
                .query_opt(
                    "SELECT type_id, name FROM sde.inv_types WHERE published AND name ILIKE '%' || $1 || '%'
                     ORDER BY lower(name) = lower($1) DESC, length(name) LIMIT 1",
                    &[&arg],
                )
                .await
        }
    }
    .map_err(db_err)?;
    row.map(|r| (r.get(0), r.get(1))).ok_or_else(|| slash_error(format!("no such type: {arg}")))
}

/// /tr me <destination>
async fn tr(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let dest = match args {
        ["me", dest, ..] => *dest,
        [dest] if *dest != "me" => *dest,
        _ => return Err(slash_error("usage: /tr me <itemID|characterID|solarSystemID>".into())),
    };
    let Ok(id) = dest.parse::<i64>() else { return Err(slash_error(format!("not an ID: {dest}"))) };
    if c.session.int("stationid").is_some() {
        return Err(slash_error("undock first".into()));
    }
    let here = c.session.int("solarsystemid").unwrap_or(0);
    // Something in this system's ballpark.
    if let Some((ball_pos, radius)) = c.world.with(here, move |s| s.ball(id).map(|b| (b.pos, b.radius))).await.flatten() {
        let pos = [ball_pos[0] + radius as f64 + 5000.0, ball_pos[1], ball_pos[2]];
        super::ship::teleport(c, here, pos).await?;
        return Ok(format!("teleported to {id}"));
    }
    // A solar system: off one of its planets (or its sun).
    if (30_000_000..33_000_000).contains(&id) {
        let p = c
            .db
            .client
            .query_opt(
                "SELECT x, y, z, COALESCE(radius, 10000)::float8 FROM sde.map_celestials WHERE solar_system_id = $1
                 ORDER BY group_id = 7 DESC, random() LIMIT 1",
                &[&(id as i32)],
            )
            .await
            .map_err(db_err)?
            .ok_or_else(|| slash_error(format!("unknown solar system {id}")))?;
        let pos = [p.get::<_, f64>(0) + p.get::<_, f64>(3) + 100_000.0, p.get(1), p.get(2)];
        super::ship::teleport(c, id, pos).await?;
        return Ok(format!("teleported to system {id}"));
    }
    // A station, planet, moon, gate...
    if let Some(p) = c
        .db
        .client
        .query_opt("SELECT solar_system_id::bigint, x, y, z, COALESCE(radius, 5000)::float8 FROM sde.map_celestials WHERE item_id = $1", &[&id])
        .await
        .map_err(db_err)?
    {
        let pos = [p.get::<_, f64>(1) + p.get::<_, f64>(4) + 5000.0, p.get(2), p.get(3)];
        super::ship::teleport(c, p.get(0), pos).await?;
        return Ok(format!("teleported to {id}"));
    }
    // A character in space, or an item floating somewhere.
    let at = c
        .db
        .client
        .query_opt(
            "SELECT i.location_id, i.x, i.y, i.z FROM game.items i
             WHERE i.item_id = COALESCE((SELECT ship_id FROM game.characters WHERE character_id = $1), $1)
               AND i.x IS NOT NULL",
            &[&id],
        )
        .await
        .map_err(db_err)?;
    if let Some(r) = at {
        let system: i64 = r.get(0);
        let pos = [r.get::<_, f64>(1) + 5000.0, r.get(2), r.get(3)];
        let live = c.world.with(system, move |s| s.pilot_ball(id).or_else(|| s.ball(id)).map(|b| b.pos)).await.flatten();
        let pos = live.map(|p| [p[0] + 5000.0, p[1], p[2]]).unwrap_or(pos);
        super::ship::teleport(c, system, pos).await?;
        return Ok(format!("teleported to {id} in {system}"));
    }
    Err(slash_error(format!("can't find {id}")))
}

async fn create(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let Some(what) = args.first() else { return Err(slash_error("usage: /create <typeID|name> [qty]".into())) };
    let qty: i32 = args.get(1).and_then(|q| q.parse().ok()).unwrap_or(1).clamp(1, 1_000_000_000);
    let (type_id, name) = resolve_type(c, what).await?;
    let char_id = c.session.int("charid").unwrap_or(0);
    let (location, flag) = match (c.session.int("stationid"), c.session.int("shipid")) {
        (Some(station), _) => (station, FLAG_HANGAR),
        (None, Some(ship)) => (ship, FLAG_CARGO),
        _ => return Err(slash_error("nowhere to put it".into())),
    };
    super::combat::deposit(c, char_id, location, flag, type_id, qty, 0).await?;
    Ok(format!("created {qty} x {name} ({type_id})"))
}

async fn isk(c: &mut Client, args: &[&str]) -> Result<String, CallError> {
    let amount: f64 = args.first().and_then(|a| a.replace(',', "").parse().ok()).ok_or_else(|| slash_error("usage: /isk <amount>".into()))?;
    let char_id = c.session.int("charid").unwrap_or(0);
    let balance: f64 = c
        .db
        .client
        .query_one("UPDATE game.characters SET balance = balance + $2::float8::numeric WHERE character_id = $1 RETURNING balance::float8", &[&char_id, &amount])
        .await
        .map_err(db_err)?
        .get(0);
    c.notify("OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
    Ok(format!("balance {balance:.2} ISK"))
}

async fn heal(c: &mut Client) -> Result<String, CallError> {
    let (Some(system), Some(ship)) = (c.session.int("solarsystemid"), c.session.int("shipid")) else { return Ok("docked ships repair themselves".into()) };
    super::combat::repair(ship);
    c.world.set_health(system, ship, [1.0; 3]);
    super::ship::send_state(c).await?;
    Ok("repaired".into())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "slash" || call.method != "SlashCmd" {
        return None;
    }
    if c.login.role & GM_ROLES == 0 {
        return Some(Err(slash_error("you are not a GM".into())));
    }
    let line = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let line = line.trim().trim_start_matches('/');
    let mut words = line.split_whitespace();
    let command = words.next().unwrap_or("").to_lowercase();
    let args: Vec<&str> = words.collect();
    if command.is_empty() {
        let names: Vec<String> = COMMANDS.iter().map(|(n, _)| format!("'{n}'")).collect();
        return Some(Err(slash_error(format!("commands: [{}]", names.join(", ")))));
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
        other => Err(slash_error(format!("unknown command /{other}\n{}", help()))),
    };
    Some(result.map(Value::unicode))
}
