//! GM slash commands (the `slash` service). Chat sends `/command args` here when the
//! session has a slash role (ROLE_GML | ROLE_LEGIONEER, chatSlashCmds); the client's
//! own svc.slash handles its local commands first and forwards the rest, and the GM
//! context menu sends `/tr me <itemID>`.
//!
//! `SlashCmd('/')` answers with the command list in a UserError's `reason`
//! (svc_slash.FetchRemoteCommandList evals what follows ': ').
//! Wallet commands write exact values through the character and account DAOs to
//! `game.characters` and `game.accounts`; client wallet displays are clamped to
//! their protocol limits. Corporation wallets and target writes are unsupported.
//!
//! Type and celestial data come from the startup SDE snapshot; item positions and
//! wallet balances come from game data. Unsupported commands return a slash error.
//! Teleports to live balls use the system actor's sampled position; stored positions remain unchanged.

use super::{db_err, Call, CallError, CallResult};
use super::sql_pattern::matches_pattern;
use crate::client::Client;
use crate::consts::{CLIENT_MAX_ISK, CLIENT_MAX_PLEX, FLAG_CARGO, FLAG_HANGAR, GM_ROLES, GROUP_PLANET};
use crate::messages;
use marshal::Value;
use tracing::info;

/// Server design: clearance in metres when teleporting to an item.
const TELEPORT_CLEARANCE: f64 = 5000.0;

/// `game_schema.sql`: character balance is numeric(36, 2).
const ISK_INTEGER_DIGITS: usize = 34;
/// `game_schema.sql`: character balance is numeric(36, 2).
const ISK_DECIMALS: usize = 2;
/// `game_schema.sql`: account PLEX balance is numeric(36, 0).
const PLEX_DIGITS: usize = 36;

const COMMANDS: [(&str, &str); 12] = [
    ("tr", messages::GM_HELP_TELEPORT),
    ("wh", messages::GM_HELP_WORMHOLE),
    ("scanall", messages::GM_HELP_SCAN_ALL),
    ("sites", messages::GM_HELP_SITES),
    ("create", messages::GM_HELP_CREATE),
    ("createitem", messages::GM_HELP_CREATE_ITEM),
    ("setisk", messages::GM_HELP_SET_ISK),
    ("addisk", messages::GM_HELP_ADD_ISK),
    ("setplex", messages::GM_HELP_SET_PLEX),
    ("addplex", messages::GM_HELP_ADD_PLEX),
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
        Ok(id) => c.sde.types.get(id),
        Err(_) => {
            let lower = crate::dao::slash::lower_pattern(&c.db, arg).await.map_err(db_err)?;
            let pattern = format!("%{lower}%");
            c.sde.types.iter().filter_map(|t| {
                let name = t.name.as_ref()?;
                if t.published != Some(true) { return None; }
                let folded = t.lower_name.as_ref()?;
                matches_pattern(folded, &pattern).then_some((t, *folded == lower, name.chars().count()))
            }).min_by_key(|(t, exact, len)| (!exact, *len, t.type_id)).map(|(t, _, _)| t)
        }
    };
    row.map(|t| (t.type_id, t.name.as_ref().expect("type name").clone())).ok_or_else(|| slash_error(messages::gm_unknown_type(arg)))
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
    if let Some((ball_pos, radius)) = c.world.with(here, move |s| s.ball(id).map(|b| (b.position_at(s.now_ms()), b.radius))).await.flatten() {
        let pos = [ball_pos[0] + radius as f64 + TELEPORT_CLEARANCE, ball_pos[1], ball_pos[2]];
        super::ship::teleport(c, here, pos).await?;
        return Ok(messages::gm_teleported(id));
    }
    // SQL's descending boolean order puts NULL group IDs before planets.
    if (30_000_000..33_000_000).contains(&id) {
        let priority = c.sde.map.celestials(id)
            .map(|p| (p.group_id.is_none(), p.group_id == Some(i64::from(GROUP_PLANET))))
            .max();
        let candidates = || c.sde.map.celestials(id).filter(|p| {
            Some((p.group_id.is_none(), p.group_id == Some(i64::from(GROUP_PLANET)))) == priority
        });
        let count = candidates().count();
        let p = candidates().nth((super::rand01() * count as f64) as usize)
            .ok_or_else(|| slash_error(messages::gm_unknown_system(id)))?;
        let p = (p.x.expect("celestial x"), p.y.expect("celestial y"), p.z.expect("celestial z"), p.radius.unwrap_or(10_000.0));
        let pos = [p.0 + p.3 + 100_000.0, p.1, p.2];
        super::ship::teleport(c, id, pos).await?;
        return Ok(messages::gm_teleported_to_system(id));
    }
    // A station, planet, moon, gate...
    if let Some(p) = c.sde.map.celestial(id) {
        let pos = [p.x.expect("celestial x") + p.radius.unwrap_or(5_000.0) + TELEPORT_CLEARANCE, p.y.expect("celestial y"), p.z.expect("celestial z")];
        super::ship::teleport(c, p.solar_system_id.expect("celestial system"), pos).await?;
        return Ok(messages::gm_teleported(id));
    }
    // A character in space, or an item floating somewhere.
    let at = crate::dao::items::social::position(&c.db, id).await.map_err(db_err)?;
    if let Some(r) = at {
        let system = r.location_id;
        let pos = [r.x + TELEPORT_CLEARANCE, r.y, r.z];
        let live = c.world.with(system, move |s| s.pilot_ball(id).or_else(|| s.ball(id)).map(|b| b.position_at(s.now_ms()))).await.flatten();
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
    super::items::deposit(&c.db, &c.sde, &mut outbox, char_id, location, flag, type_id, qty, 0).await?;
    crate::db::outbox::dispatch_client((), outbox, c)?;
    Ok(messages::gm_created_item(qty, &name, type_id))
}

fn parse_amount(args: &[&str], integer_digits: usize, decimals: usize) -> Option<String> {
    let [arg] = args else { return None };
    let amount = arg.replace(',', "");
    let (integer, fraction) = match amount.split_once('.') {
        Some((integer, fraction)) if decimals > 0 => (integer, Some(fraction)),
        Some(_) => return None,
        None => (amount.as_str(), None),
    };
    if integer.is_empty() || integer.len() > integer_digits || !integer.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(fraction) = fraction {
        if fraction.is_empty() || fraction.len() > decimals || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    Some(amount)
}

fn exceeds_cap(exact: &str, cap: &str) -> bool {
    let (integer, fraction) = exact.split_once('.').unwrap_or((exact, ""));
    let integer = integer.trim_start_matches('0');
    let cap = cap.trim_start_matches('0');
    integer.len() > cap.len()
        || (integer.len() == cap.len() && (integer > cap
            || (integer == cap && fraction.bytes().any(|b| b != b'0'))))
}

fn wallet_error(error: crate::db::Error) -> CallError {
    if matches!(&error, crate::db::Error::Postgres(e)
        if e.code() == Some(&tokio_postgres::error::SqlState::NUMERIC_VALUE_OUT_OF_RANGE))
    {
        slash_error(messages::GM_AMOUNT_TOO_LARGE.into())
    } else {
        db_err(error)
    }
}

async fn wallet(c: &mut Client, args: &[&str], command: &str) -> Result<String, CallError> {
    let (usage, digits, decimals) = match command {
        "setisk" => (messages::GM_SET_ISK_USAGE, ISK_INTEGER_DIGITS, ISK_DECIMALS),
        "addisk" => (messages::GM_ADD_ISK_USAGE, ISK_INTEGER_DIGITS, ISK_DECIMALS),
        "setplex" => (messages::GM_SET_PLEX_USAGE, PLEX_DIGITS, 0),
        "addplex" => (messages::GM_ADD_PLEX_USAGE, PLEX_DIGITS, 0),
        _ => unreachable!(),
    };
    let amount = parse_amount(args, digits, decimals).ok_or_else(|| slash_error(usage.into()))?;
    let _guard = super::market::lock().await;
    match command {
        "setisk" | "addisk" => {
            let char_id = c.session.int("charid").unwrap_or(0);
            let (exact, balance) = if command == "setisk" {
                crate::dao::wallets::characters::set_exact(&c.db, char_id, &amount).await
            } else {
                crate::dao::wallets::characters::credit_exact(&c.db, char_id, &amount).await
            }.map_err(wallet_error)?;
            c.notify("OnAccountChange", vec![Value::str("cash"), Value::Int(char_id), Value::Float(balance)]);
            let shown = exceeds_cap(&exact, &format!("{CLIENT_MAX_ISK:.0}")).then_some(balance);
            Ok(messages::gm_balance(&exact, shown))
        }
        "setplex" | "addplex" => {
            let (exact, balance) = if command == "setplex" {
                crate::dao::accounts::balances::set_plex(&c.db, c.login.user_id, &amount).await
            } else {
                crate::dao::accounts::balances::add_plex(&c.db, c.login.user_id, &amount).await
            }.map_err(wallet_error)?;
            super::store::notify_plex_balance(c, balance);
            let shown = exceeds_cap(&exact, &CLIENT_MAX_PLEX.to_string()).then_some(balance);
            Ok(messages::gm_plex_balance(&exact, shown))
        }
        _ => unreachable!(),
    }
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
        "setisk" | "addisk" | "setplex" | "addplex" => wallet(c, &args, &command).await,
        "heal" => heal(c).await,
        "help" => Ok(help()),
        other => Err(slash_error(messages::gm_unknown_command(other, &help()))),
    };
    Some(result.map(Value::unicode))
}

#[cfg(test)]
mod tests;
