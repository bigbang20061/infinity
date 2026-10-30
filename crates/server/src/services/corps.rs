//! Player corporations (corpRegistry, bound per corporation; eve/client/script/ui/services/corporation).
//!
//! A character with Corporation Management can found a corporation (game.corporations, IDs
//! from 98,000,000 like the live server's) for appConst.corporationStartupCost while docked,
//! and becomes its CEO with every role. Others join by application (or invitation) that the
//! corporation's personnel managers, or the character, accept; members leave, or directors
//! kick them, back to their school's NPC corporation. Membership changes reach an online
//! pilot as a session change (corpid, corprole...), which also moves them to the new corp
//! chat channel. A member's roles are one mask (game.characters.corp_roles) used for all
//! locations. Alliances are services/alliances; titles, wallet divisions and offices are
//! supported. Shares aren't supported.

use super::{db_err, keyval, notify_error, row, rowset, trimmed_arg_text, value_id_list, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, CORPORATION_TYPE_PLAYER, CORPORATION_UNLIMITED_MEMBER_LIMIT, CORP_DIVISION_NAME_COUNT, FILETIME_SECOND, FILETIME_UNIX_EPOCH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH, ROLE_DIRECTOR, TYPE_CHARACTER};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::info;

/// appConst.corporationStartupCost
const STARTUP_COST: f64 = 1_599_800.0;
/// SDE invTypes: Corporation Management skill.
const SKILL_CORPORATION_MANAGEMENT: i32 = 3363;
/// Server policy: minimum Corporation Management skill level for founding or leading a corporation.
const MIN_CORPORATION_MANAGEMENT_LEVEL: i32 = 1;
/// Client appConst.corpRolePersonnelManager.
const ROLE_PERSONNEL_MANAGER: i64 = 128;
/// Server policy: the CEO holds every role.
const CEO_ROLES: i64 = i64::MAX;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_APPLIED: i32 = 0;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_RENEGOTIATED_BY_CHARACTER: i32 = 1;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CHARACTER: i32 = 2;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_REJECTED_BY_CHARACTER: i32 = 3;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_RENEGOTIATED_BY_CORPORATION: i32 = 5;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_ACCEPTED_BY_CORPORATION: i32 = 6;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_WITHDRAWN_BY_CHARACTER: i32 = 7;
/// Server application state used by eve/client/script/ui/services/corporation.
const APP_INVITED_BY_CORPORATION: i32 = 8;
/// Server definition of application states that remain open.
const OPEN_APPLICATION: [i32; 4] = [APP_APPLIED, APP_RENEGOTIATED_BY_CHARACTER, APP_RENEGOTIATED_BY_CORPORATION, APP_INVITED_BY_CORPORATION];
/// Client appConst.CTV_ADD.
const CTV_ADD: i64 = 1;
/// Client appConst.CTV_REMOVE.
const CTV_REMOVE: i64 = 2;
/// Client appConst.CTV_SET.
const CTV_SET: i64 = 3;
/// Client corporation title mask: sixteen title bits.
const TITLE_COUNT: usize = 16;
/// Server policy: maximum returned historical corporation applications.
const OLD_APPLICATION_LIMIT: i64 = 100;
/// Server corporation header policy: initial number of shares.
const INITIAL_SHARES: i64 = 1000;
/// Player corporation IDs (idCheckers.IsPlayerCorporation).
const FIRST_PLAYER_CORP: i64 = 98_000_000;

pub fn is_player_corp(id: i64) -> bool {
    id >= FIRST_PLAYER_CORP
}

/// Server title model: union the member's role bits with the roles of all held titles.
pub(super) const EFFECTIVE_ROLES_SQL: &str = "(ch.corp_roles | COALESCE((SELECT bit_or(t.roles) FROM game.corp_titles t
     WHERE t.corporation_id = ch.corporation_id AND ch.corp_title_mask & t.title_id <> 0), 0))";

/// The corporation `char_id` is in, and their role mask (titles included).
pub(super) async fn membership(db: &(impl Executor + ?Sized), char_id: i64) -> Result<(i64, i64), CallError> {
    let r = db
        .query_one(&format!("SELECT ch.corporation_id, {EFFECTIVE_ROLES_SQL} FROM game.characters ch WHERE ch.character_id = $1"), &[&char_id])
        .await
        .map_err(db_err)?;
    Ok((r.get(0), r.get(1)))
}

/// Send an online member their current roles.
async fn refresh_roles(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    let (_, roles) = membership(db, char_id).await?;
    let pair = |k: &str| Value::tuple([Value::str(k), Value::Int(roles)]);
    world.notify_pilot(char_id, "__session", vec![pair("corprole"), pair("rolesAtAll"), pair("rolesAtBase"), pair("rolesAtHQ"), pair("rolesAtOther")]);
    Ok(())
}

pub(super) async fn ceo_of(db: &(impl Executor + ?Sized), corp: i64) -> Result<Option<i64>, CallError> {
    Ok(db
        .query_opt("SELECT ceo_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await
        .map_err(db_err)?
        .and_then(|r| r.get(0)))
}

/// Move a character into `corp` with `roles`, recording the employment and updating their
/// session if they're online.
async fn set_corp<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, corp: i64, roles: i64, now: i64) -> Result<(), CallError> {
    let (hq, alliance): (Option<i64>, Option<i64>) = if is_player_corp(corp) {
        db
            .query_opt("SELECT station_id, alliance_id FROM game.corporations WHERE corporation_id = $1", &[&corp])
            .await
            .map_err(db_err)?
            .map(|r| (r.get(0), r.get(1)))
            .unwrap_or_default()
    } else {
        let hq = db
            .query_opt("SELECT station_id::bigint FROM sde.crp_npc_corporations WHERE corporation_id = $1::bigint", &[&corp])
            .await
            .map_err(db_err)?
            .and_then(|r| r.get(0));
        (hq, None)
    };
    db
        .execute(
            "UPDATE game.characters SET corporation_id = $2, corp_roles = $3, corp_grantable_roles = 0, corp_title_mask = 0, corp_title = ''
             WHERE character_id = $1",
            &[&char_id, &corp, &roles],
        )
        .await
        .map_err(db_err)?;
    db
        .execute("INSERT INTO game.employment_history (char_id, corporation_id, start_date) VALUES ($1, $2, $3)", &[&char_id, &corp, &now])
        .await
        .map_err(db_err)?;
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    outbox.notify_pilot(
        char_id,
        "__session",
        vec![
            pair("corpid", Value::Int(corp)),
            pair("allianceid", alliance.map(Value::Int).unwrap_or(Value::None)),
            pair("corprole", Value::Int(roles)),
            pair("rolesAtAll", Value::Int(roles)),
            pair("rolesAtBase", Value::Int(roles)),
            pair("rolesAtHQ", Value::Int(roles)),
            pair("rolesAtOther", Value::Int(roles)),
            pair("hqID", hq.map(Value::Int).unwrap_or(Value::None)),
        ],
    );
    Ok(())
}

/// The NPC corporation of the character's school.
async fn school_corp(db: &(impl Executor + ?Sized), char_id: i64) -> Result<i64, CallError> {
    let r = db
        .query_one(
            "SELECT COALESCE(
                 (SELECT (s.data->>'corporationID')::bigint FROM sde_native.schools s WHERE s.key = ch.school_id::text),
                 (SELECT (s.data->>'corporationID')::bigint FROM sde_native.schools s WHERE (s.data->>'raceID')::int = ch.race_id ORDER BY s.key::int LIMIT 1))
             FROM game.characters ch WHERE ch.character_id = $1",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    r.get::<_, Option<i64>>(0).ok_or_else(|| CallError::Internal(anyhow::anyhow!("no school corporation for {char_id}")))
}

/// `AddCorporation(name, ticker, description, url, taxRate, shape1..3, color1..3, typeface,
/// applicationsEnabled, friendlyFire, lpTaxRate)` -> corporationID.
async fn found(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
    let (name, ticker, description, url) = (trimmed_arg_text(call, 0), trimmed_arg_text(call, 1).to_uppercase(), trimmed_arg_text(call, 2), trimmed_arg_text(call, 3));
    let tax = call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error(messages::STATION_DOCK_REQUIRED_TO_CREATE_CORP));
    };
    if !(ORG_NAME_MIN_LENGTH..=ORG_NAME_MAX_LENGTH).contains(&name.chars().count()) {
        return Err(notify_error(messages::CORP_NAME_LENGTH_INVALID));
    }
    if !(ORG_SHORT_NAME_MIN_LENGTH..=ORG_SHORT_NAME_MAX_LENGTH).contains(&ticker.chars().count()) || !ticker.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(notify_error(messages::CORP_SHORT_NAME_INVALID));
    }
    let skill: i32 = db
        .query_opt("SELECT level FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&me, &SKILL_CORPORATION_MANAGEMENT])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0);
    if skill < MIN_CORPORATION_MANAGEMENT_LEVEL {
        return Err(notify_error(messages::CORPORATION_MANAGEMENT_SKILL_REQUIRED));
    }
    let (current, _) = membership(db, me).await?;
    if is_player_corp(current) && ceo_of(db, current).await? == Some(me) {
        return Err(notify_error(messages::ALREADY_CORP_CEO_MUST_RESIGN_OR_CLOSE));
    }
    let taken: bool = db
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM game.corporations WHERE NOT deleted AND (lower(name) = lower($1) OR upper(ticker) = $2))
                 OR EXISTS (SELECT 1 FROM sde.crp_npc_corporations WHERE lower(name) = lower($1) OR upper(ticker) = $2)",
            &[&name, &ticker],
        )
        .await
        .map_err(db_err)?
        .get(0);
    if taken {
        return Err(notify_error(messages::CORP_NAME_OR_SHORT_NAME_TAKEN));
    }
    let _trading = super::market::lock().await;
    let now = filetime_now();
    let recruiting = call.args.get(12).and_then(|v| v.as_int()).unwrap_or(1) != 0;
    let friendly = matches!(call.args.get(13), Some(Value::Bool(true)));
    let shapes = [int(5), int(6), int(7), int(8), int(9), int(10), int(11)];
    let id = db.transaction(
        |tx, outbox| {
            let (name, ticker, description, url) = (name.clone(), ticker.clone(), description.clone(), url.clone());
            Box::pin(async move {
                super::market::debit(tx, outbox, me, STARTUP_COST).await?;
                let id: i64 = tx.query_one(
                    "INSERT INTO game.corporations (name, ticker, description, url, tax_rate, ceo_id, creator_id, station_id, is_recruiting, friendly_fire,
                                                    shape1, shape2, shape3, color1, color2, color3, typeface, created)
                     VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING corporation_id",
                    &[&name, &ticker, &description, &url, &tax, &me, &station, &recruiting, &friendly,
                      &shapes[0], &shapes[1], &shapes[2], &shapes[3], &shapes[4], &shapes[5], &shapes[6], &now],
                ).await.map_err(db_err)?.get(0);
                set_corp(tx, outbox, me, id, CEO_ROLES, now).await?;
                Ok(id)
            })
        },
        |id, outbox| async move { dispatch_world(id, outbox, world) },
    ).await?;
    info!("corporations: {me} founded {id} [{ticker}] {name}");
    Ok(Value::Int(id))
}

/// Leave (or be kicked from) a player corporation, back to the school's NPC corporation.
/// A CEO can only leave as the last member, which closes the corporation.
async fn leave_inner<X: Send>(db: &(impl Executor + ?Sized), outbox: &mut Outbox<X>, char_id: i64, now: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, char_id).await?;
    if !is_player_corp(corp) {
        return Err(notify_error(messages::CANNOT_LEAVE_NPC_CORP));
    }
    if ceo_of(db, corp).await? == Some(char_id) {
        let others: i64 = db
            .query_one("SELECT count(*) FROM game.characters WHERE corporation_id = $1 AND character_id <> $2 AND NOT deleted", &[&corp, &char_id])
            .await
            .map_err(db_err)?
            .get(0);
        if others > 0 {
            return Err(notify_error(messages::CORP_CEO_CANNOT_LEAVE_WITH_MEMBERS));
        }
        super::alliances::leave_alliance(db, outbox, corp, now).await?;
        db.execute("UPDATE game.corporations SET deleted = true, ceo_id = NULL WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?;
    }
    let school = school_corp(db, char_id).await?;
    set_corp(db, outbox, char_id, school, 0, now).await
}

async fn leave(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    let now = filetime_now();
    db.transaction(
        |tx, outbox| Box::pin(async move { leave_inner(tx, outbox, char_id, now).await }),
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await
}

fn application_row(r: &tokio_postgres::Row) -> Value {
    keyval([
        ("applicationID", Value::Int(r.get(0))),
        ("corporationID", Value::Int(r.get(1))),
        ("characterID", Value::Int(r.get(2))),
        ("applicationText", Value::unicode(r.get::<_, String>(3))),
        ("status", Value::Int(r.get::<_, i32>(4) as i64)),
        ("applicationDateTime", Value::Int(r.get(5))),
        ("customMessage", Value::unicode(r.get::<_, String>(6))),
    ])
}

const APPLICATION_SELECT: &str =
    "SELECT application_id, corporation_id, character_id, application_text, status, created, custom_message FROM game.corp_applications";

/// Open applications grouped by `key` (corporation or character): {id: [application]}.
async fn applications(db: &Db, where_sql: &str, arg: i64, by_corp: bool) -> CallResult {
    let rows = db
        .query(&format!("{APPLICATION_SELECT} WHERE {where_sql} AND status = ANY($2) ORDER BY application_id"), &[&arg, &OPEN_APPLICATION.to_vec()])
        .await
        .map_err(db_err)?;
    let mut grouped: Vec<(i64, Vec<Value>)> = vec![];
    for r in &rows {
        let key: i64 = if by_corp { r.get(1) } else { r.get(2) };
        match grouped.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(application_row(r)),
            None => grouped.push((key, vec![application_row(r)])),
        }
    }
    Ok(Value::Dict(grouped.into_iter().map(|(k, v)| (Value::Int(k), Value::List(v))).collect()))
}

/// `UpdateApplicationOffer(applicationID, characterID, corporationID, text, status, customMessage, date)`.
async fn update_application(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let (app_id, status) = (int(0), int(4) as i32);
    let message = call.args.get(5).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
    let Some(r) = db.query_opt(&format!("{APPLICATION_SELECT} WHERE application_id = $1"), &[&app_id]).await.map_err(db_err)? else {
        return Err(notify_error(messages::ALLIANCE_APPLICATION_NOT_FOUND));
    };
    let (corp, applicant, current): (i64, i64, i32) = (r.get(1), r.get(2), r.get(4));
    if !OPEN_APPLICATION.contains(&current) {
        return Err(notify_error(messages::CORP_APPLICATION_ALREADY_PROCESSED));
    }
    let by_character = matches!(status, APP_ACCEPTED_BY_CHARACTER | APP_REJECTED_BY_CHARACTER | APP_WITHDRAWN_BY_CHARACTER | APP_RENEGOTIATED_BY_CHARACTER);
    if by_character && applicant != me {
        return Err(notify_error(messages::CORP_APPLICATION_NOT_OWNED));
    }
    if !by_character {
        let (my_corp, roles) = membership(db, me).await?;
        if my_corp != corp || roles & (ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR) == 0 {
            return Err(notify_error(messages::PERSONNEL_MANAGER_REQUIRED_FOR_APPLICATION));
        }
    }
    let joins = (status == APP_ACCEPTED_BY_CORPORATION && current != APP_INVITED_BY_CORPORATION)
        || (status == APP_ACCEPTED_BY_CHARACTER && current == APP_INVITED_BY_CORPORATION);
    let now = filetime_now();
    db.transaction(
        |tx, outbox| {
            let message = message.clone();
            Box::pin(async move {
                let affected = tx.execute("UPDATE game.corp_applications SET status = $2, custom_message = $3 WHERE application_id = $1 AND status = $4", &[&app_id, &status, &message, &current])
                    .await.map_err(db_err)?;
                if affected == 0 {
                    return Err(notify_error(messages::CORP_APPLICATION_ALREADY_PROCESSED));
                }
                if joins {
                    let (old, _) = membership(tx, applicant).await?;
                    if is_player_corp(old) && ceo_of(tx, old).await? == Some(applicant) {
                        return Err(notify_error(messages::CEO_MUST_LEAVE_CORP_BEFORE_JOINING));
                    }
                    set_corp(tx, outbox, applicant, corp, 0, now).await?;
                    tx.execute("UPDATE game.corp_applications SET status = $3 WHERE character_id = $1 AND application_id <> $2 AND status = ANY($4)", &[&applicant, &app_id, &APP_WITHDRAWN_BY_CHARACTER, &OPEN_APPLICATION.to_vec()])
                        .await.map_err(db_err)?;
                }
                Ok(())
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, world) },
    ).await?;
    Ok(Value::None)
}

/// A member row (bco_members, corp_ui_member_*).
fn member_row(r: &tokio_postgres::Row, corp: i64) -> Value {
    let (roles, grantable): (i64, i64) = (r.get(1), r.get(7));
    keyval([
        ("characterID", Value::Int(r.get(0))),
        ("corporationID", Value::Int(corp)),
        ("roles", Value::Int(roles)),
        ("grantableRoles", Value::Int(grantable)),
        ("rolesAtHQ", Value::Int(roles)),
        ("grantableRolesAtHQ", Value::Int(grantable)),
        ("rolesAtBase", Value::Int(roles)),
        ("grantableRolesAtBase", Value::Int(grantable)),
        ("rolesAtOther", Value::Int(roles)),
        ("grantableRolesAtOther", Value::Int(grantable)),
        ("baseID", Value::None),
        ("titleMask", Value::Int(r.get(8))),
        ("title", Value::unicode(r.get::<_, String>(9))),
        ("startDateTime", Value::Int(r.get(2))),
        ("divisionID", Value::Int(0)),
        ("squadronID", Value::Int(0)),
        ("blockRoles", Value::None),
        ("accountKey", Value::Int(ACCOUNT_CASH as i64)),
    ])
}

const MEMBER_SELECT: &str = "SELECT ch.character_id, ch.corp_roles,
        COALESCE((SELECT max(e.start_date) FROM game.employment_history e WHERE e.char_id = ch.character_id AND e.corporation_id = ch.corporation_id),
                 (extract(epoch FROM ch.created_at) * $3::bigint)::bigint + $4::bigint),
        ch.station_id, ch.solar_system_id, (extract(epoch FROM ch.logoff_at) * $3::bigint)::bigint + $4::bigint,
        (SELECT type_id FROM game.items WHERE item_id = ch.ship_id), ch.corp_grantable_roles, ch.corp_title_mask, ch.corp_title
    FROM game.characters ch";

async fn members(db: &Db, corp: i64, extra: &str, ids: &[i64]) -> Result<Vec<tokio_postgres::Row>, CallError> {
    db
        .query(&format!("{MEMBER_SELECT} WHERE ch.corporation_id = $1 AND NOT ch.deleted {extra} ORDER BY ch.character_id"), &[&corp, &ids.to_vec(), &FILETIME_SECOND, &FILETIME_UNIX_EPOCH])
        .await
        .map_err(db_err)
}

/// The corporation a bound corpRegistry is for (its bindParams), else the session's.
fn bound_corp(c: &Client, call: &Call<'_>) -> i64 {
    call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(c.session.int("corpid").unwrap_or(0))
}

/// Employment history rows for the character info window: (corporationID, deleted, startDate).
pub async fn employment_rows(db: &Db, char_id: i64) -> Result<Vec<Value>, CallError> {
    let rows = db
        .query(
            "SELECT corporation_id, start_date FROM game.employment_history WHERE char_id = $1
             UNION ALL
             SELECT ch.corporation_id, (extract(epoch FROM ch.created_at) * $2::bigint)::bigint + $3::bigint
             FROM game.characters ch WHERE ch.character_id = $1 AND NOT EXISTS (SELECT 1 FROM game.employment_history WHERE char_id = $1)
             ORDER BY 2 DESC",
            &[&char_id, &FILETIME_SECOND, &FILETIME_UNIX_EPOCH],
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let corp: i64 = r.get(0);
            keyval([("corporationID", Value::Int(corp)), ("deleted", Value::Bool(false)), ("startDate", Value::Int(r.get(1)))])
        })
        .collect())
}

/// The CORP_HEADER line of a player corporation (universe::corporation).
pub async fn corporation_line(db: &Db, corp: i64) -> Result<Option<Vec<Value>>, CallError> {
    let Some(r) = db
        .query_opt(
            "SELECT name, description, ticker, url, tax_rate, lp_tax_rate, creator_id, ceo_id, station_id, is_recruiting, friendly_fire,
                    shape1::bigint, shape2::bigint, shape3::bigint, color1::bigint, color2::bigint, color3::bigint, typeface::bigint, created, deleted,
                    (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = $1 AND NOT ch.deleted), alliance_id, chosen_executor_id, division_names
             FROM game.corporations WHERE corporation_id = $1",
            &[&corp],
        )
        .await
        .map_err(db_err)?
    else {
        return Ok(None);
    };
    let opt = |i: usize| r.get::<_, Option<i64>>(i).map(Value::Int).unwrap_or(Value::None);
    let s = |i: usize| Value::unicode(r.get::<_, String>(i));
    let mut line = vec![
        Value::Int(corp),
        s(0),
        s(1),
        s(2),
        s(3),
        Value::Float(r.get(4)),
        Value::Float(r.get(5)),
        Value::Float(0.0),
        Value::Int(CORPORATION_TYPE_PLAYER),
        Value::Bool(true),
        Value::Bool(true),
        Value::Int(r.get(6)),
        opt(7),
        Value::Int(r.get(8)),
        Value::None,
        opt(21),
        Value::None,
        Value::Int(INITIAL_SHARES),
        Value::Int(r.get(20)),
        Value::Int(CORPORATION_UNLIMITED_MEMBER_LIMIT),
        Value::None,
        Value::None,
    ];
    line.extend((11..18).map(opt));
    // Hangar then wallet division names; None for the client's defaults.
    let divisions: Vec<Option<String>> = r.get::<_, Option<Vec<Option<String>>>>(23).unwrap_or_default();
    line.extend((0..CORP_DIVISION_NAME_COUNT).map(|i| divisions.get(i).cloned().flatten().filter(|n| !n.is_empty()).map(Value::unicode).unwrap_or(Value::None)));
    // ..., chosenExecutorID, allowWar: wars can't be declared yet.
    line.extend([Value::Bool(r.get(19)), Value::Bool(r.get(9)), Value::Bool(r.get(10)), Value::None, Value::Int(r.get(18)), opt(22), Value::Bool(false)]);
    Ok(Some(line))
}

// ---------------------------------------------------------------- roles and titles

/// The lines of a `Rowset` argument (UpdateMembers, UpdateTitles).
fn rowset_lines(v: Option<&Value>) -> Vec<Vec<Value>> {
    let lines = match v {
        Some(Value::Instance(_, state)) => state.get("lines").cloned(),
        Some(other) => other.get("lines").cloned().or_else(|| Some(other.clone())),
        None => None,
    };
    lines.and_then(|l| l.items().map(|it| it.iter().filter_map(|line| line.items().map(|x| x.to_vec())).collect())).unwrap_or_default()
}

/// `UpdateMember(characterID, title, divisionID, squadronID, roles, grantableRoles, rolesAtHQ,
/// grantableRolesAtHQ, rolesAtBase, grantableRolesAtBase, rolesAtOther, grantableRolesAtOther,
/// baseID, titleMask, blockRoles)`: None leaves a field as it is. Roles are one mask here, so
/// the per-location ones are folded into it. Directors edit anyone but the CEO.
async fn update_member(db: &Db, world: &World, me: i64, args: &[Value]) -> Result<(), CallError> {
    let int = |i: usize| args.get(i).and_then(|v| v.as_int());
    let target = int(0).unwrap_or(0);
    let title = args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned());
    let roles = int(4).map(|r| r | int(6).unwrap_or(0) | int(8).unwrap_or(0) | int(10).unwrap_or(0));
    let grantable = int(5).map(|g| g | int(7).unwrap_or(0) | int(9).unwrap_or(0) | int(11).unwrap_or(0));
    let title_mask = int(13);
    if roles.is_none() && grantable.is_none() && title.is_none() && title_mask.is_none() {
        return Ok(()); // e.g. only blockRoles, which isn't modelled
    }
    let (my_corp, my_roles) = membership(db, me).await?;
    let (their_corp, _) = membership(db, target).await?;
    let ceo = ceo_of(db, my_corp).await?;
    let is_ceo = ceo == Some(me);
    if my_corp != their_corp || (my_roles & ROLE_DIRECTOR == 0 && !is_ceo) || (ceo == Some(target) && !is_ceo) {
        return Err(notify_error(messages::CANNOT_MODIFY_MEMBER_ROLES));
    }
    // A CEO keeps every role.
    let roles = if ceo == Some(target) { Some(CEO_ROLES) } else { roles };
    db
        .execute(
            "UPDATE game.characters SET corp_roles = COALESCE($2, corp_roles), corp_grantable_roles = COALESCE($3, corp_grantable_roles),
                    corp_title = COALESCE($4, corp_title), corp_title_mask = COALESCE($5, corp_title_mask)
             WHERE character_id = $1",
            &[&target, &roles, &grantable, &title, &title_mask],
        )
        .await
        .map_err(db_err)?;
    refresh_roles(db, world, target).await
}

/// `ExecuteActions(targetIDs, [(verb, property, value)])`: appConst.CTV_ADD/REMOVE/SET on
/// roles, grantable roles or titles of several members (bases aren't modelled).
async fn execute_actions(db: &Db, world: &World, me: i64, call: &Call<'_>) -> Result<(), CallError> {
    let targets = value_id_list(call.args.first());
    let actions = call.args.get(1).and_then(|v| v.items()).unwrap_or(&[]);
    let (my_corp, my_roles) = membership(db, me).await?;
    let ceo = ceo_of(db, my_corp).await?;
    if my_roles & ROLE_DIRECTOR == 0 && ceo != Some(me) {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_FOR_BULK_MEMBER_EDIT));
    }
    for target in targets {
        let (their_corp, _) = membership(db, target).await?;
        if their_corp != my_corp || ceo == Some(target) {
            continue;
        }
        for action in actions {
            let Some([verb, property, value]) = action.items() else { continue };
            let column = match property.as_text().as_deref() {
                Some("roles") => "corp_roles",
                Some("grantableRoles") => "corp_grantable_roles",
                Some("titleMask") => "corp_title_mask",
                _ => continue,
            };
            let value = value.as_int().unwrap_or(0);
            let expr = match verb.as_int() {
                Some(CTV_ADD) => format!("{column} | $2"),
                Some(CTV_REMOVE) => format!("{column} & ~$2"),
                Some(CTV_SET) => "$2".to_string(),
                _ => continue,
            };
            db
                .execute(&format!("UPDATE game.characters SET {column} = {expr} WHERE character_id = $1"), &[&target, &value])
                .await
                .map_err(db_err)?;
        }
        refresh_roles(db, world, target).await?;
    }
    Ok(())
}

const TITLE_HEADER: &[&str] = &[
    "titleID",
    "titleName",
    "roles",
    "grantableRoles",
    "rolesAtHQ",
    "grantableRolesAtHQ",
    "rolesAtBase",
    "grantableRolesAtBase",
    "rolesAtOther",
    "grantableRolesAtOther",
];

/// {titleID: Row} for all 16 title bits; unnamed ones the client calls "Untitled n".
async fn titles(db: &Db, corp: i64) -> CallResult {
    let rows = db
        .query(
            "SELECT title_id, name, roles, grantable_roles, roles_hq, grantable_hq, roles_base, grantable_base, roles_other, grantable_other
             FROM game.corp_titles WHERE corporation_id = $1",
            &[&corp],
        )
        .await
        .map_err(db_err)?;
    let header = Value::list(TITLE_HEADER.iter().map(|h| Value::str(*h)));
    Ok(Value::Dict(
        (0..TITLE_COUNT)
            .map(|bit| {
                let id = 1i64 << bit;
                let line: Vec<Value> = match rows.iter().find(|r| r.get::<_, i64>(0) == id) {
                    Some(r) => std::iter::once(Value::Int(id))
                        .chain(std::iter::once(Value::unicode(r.get::<_, String>(1))))
                        .chain((2..10).map(|i| Value::Int(r.get(i))))
                        .collect(),
                    None => std::iter::once(Value::Int(id)).chain(std::iter::once(Value::unicode(""))).chain((2..10).map(|_| Value::Int(0))).collect(),
                };
                (Value::Int(id), row(header.clone(), line))
            })
            .collect(),
    ))
}

/// Online members holding any of `title_mask`'s titles get their roles again.
async fn refresh_title_holders(db: &Db, world: &World, corp: i64, title_mask: i64) -> Result<(), CallError> {
    let rows = db
        .query("SELECT character_id FROM game.characters WHERE corporation_id = $1 AND corp_title_mask & $2 <> 0 AND NOT deleted", &[&corp, &title_mask])
        .await
        .map_err(db_err)?;
    for r in rows {
        refresh_roles(db, world, r.get(0)).await?;
    }
    Ok(())
}

/// Store title lines `(titleID, titleName, roles, grantableRoles, rolesAtHQ, ...)`; only
/// directors may, and no title carries the Director role.
async fn update_titles(db: &Db, world: &World, me: i64, corp: i64, lines: Vec<Vec<Value>>) -> Result<(), CallError> {
    let (my_corp, roles) = membership(db, me).await?;
    if my_corp != corp || roles & ROLE_DIRECTOR == 0 {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_TITLES));
    }
    let mut changed = 0i64;
    for line in lines {
        let int = |i: usize| line.get(i).and_then(|v| v.as_int()).unwrap_or(0) & !ROLE_DIRECTOR;
        let id = line.first().and_then(|v| v.as_int()).unwrap_or(0);
        if id <= 0 || id.count_ones() != 1 || id > 1 << (TITLE_COUNT - 1) {
            continue;
        }
        let name = line.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
        db
            .execute(
                "INSERT INTO game.corp_titles (corporation_id, title_id, name, roles, grantable_roles, roles_hq, grantable_hq, roles_base, grantable_base, roles_other, grantable_other)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 ON CONFLICT (corporation_id, title_id) DO UPDATE SET name = $3, roles = $4, grantable_roles = $5, roles_hq = $6, grantable_hq = $7,
                     roles_base = $8, grantable_base = $9, roles_other = $10, grantable_other = $11",
                &[&corp, &id, &name, &int(2), &int(3), &int(4), &int(5), &int(6), &int(7), &int(8), &int(9)],
            )
            .await
            .map_err(db_err)?;
        changed |= id;
    }
    refresh_title_holders(db, world, corp, changed).await
}

async fn delete_title(db: &Db, world: &World, me: i64, corp: i64, title: i64) -> Result<(), CallError> {
    let (my_corp, roles) = membership(db, me).await?;
    if my_corp != corp || roles & ROLE_DIRECTOR == 0 {
        return Err(notify_error(messages::DIRECTOR_REQUIRED_TO_DELETE_TITLES));
    }
    let holders = db
        .query("SELECT character_id FROM game.characters WHERE corporation_id = $1 AND corp_title_mask & $2 <> 0", &[&corp, &title])
        .await
        .map_err(db_err)?;
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id = $1 AND title_id = $2", &[&corp, &title]).await.map_err(db_err)?;
    db
        .execute("UPDATE game.characters SET corp_title_mask = corp_title_mask & ~$2 WHERE corporation_id = $1", &[&corp, &title])
        .await
        .map_err(db_err)?;
    for r in holders {
        refresh_roles(db, world, r.get(0)).await?;
    }
    Ok(())
}

/// Members other than the CEO with Corporation Management, who could take over.
async fn potential_ceos(db: &Db, corp: i64, ceo: i64) -> Result<Vec<i64>, CallError> {
    let rows = db
        .query(
            "SELECT ch.character_id FROM game.characters ch JOIN game.skills s ON s.character_id = ch.character_id AND s.type_id = $3 AND s.level >= $4
             WHERE ch.corporation_id = $1 AND ch.character_id <> $2 AND NOT ch.deleted ORDER BY ch.character_id",
            &[&corp, &ceo, &SKILL_CORPORATION_MANAGEMENT, &MIN_CORPORATION_MANAGEMENT_LEVEL],
        )
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// `ResignFromCEO(newCeoID=None)`. Without a successor it returns the members who could be
/// one for the CEO to pick from; a CEO alone closes the corporation, and a CEO without
/// eligible successors closes it and sends everyone back to their school corporations.
async fn resign_ceo(db: &Db, world: &World, me: i64, successor: Option<i64>) -> CallResult {
    let (corp, _) = membership(db, me).await?;
    if !is_player_corp(corp) || ceo_of(db, corp).await? != Some(me) {
        return Err(CallError::User { msg: messages::MSG_ONLY_CEO_CAN_RESIGN.into(), dict: vec![] });
    }
    match successor {
        Some(next) => {
            if !potential_ceos(db, corp, me).await?.contains(&next) {
                return Err(notify_error(messages::MEMBER_CANNOT_BECOME_CEO));
            }
            db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    tx.execute("UPDATE game.corporations SET ceo_id = $2 WHERE corporation_id = $1", &[&corp, &next]).await.map_err(db_err)?;
                    let mut role_writes = [(next, CEO_ROLES), (me, ROLE_DIRECTOR)];
                    role_writes.sort_by_key(|(char_id, _)| *char_id);
                    for (char_id, roles) in role_writes {
                        tx.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&char_id, &roles]).await.map_err(db_err)?;
                    }
                    for char_id in [next, me] {
                        let (_, roles) = membership(tx, char_id).await?;
                        let pair = |k: &str| Value::tuple([Value::str(k), Value::Int(roles)]);
                        outbox.notify_pilot(char_id, "__session", vec![pair("corprole"), pair("rolesAtAll"), pair("rolesAtBase"), pair("rolesAtHQ"), pair("rolesAtOther")]);
                    }
                    Ok(())
                }),
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            info!("corporations: {me} handed {corp} to {next}");
            Ok(Value::None)
        }
        None => {
            let others: Vec<i64> = members(db, corp, "AND ch.character_id <> ALL($2)", &[me]).await?.iter().map(|r| r.get(0)).collect();
            if others.is_empty() {
                leave(db, world, me).await?;
                return Ok(Value::None);
            }
            let candidates = potential_ceos(db, corp, me).await?;
            if !candidates.is_empty() {
                return Ok(Value::list(candidates.into_iter().map(Value::Int)));
            }
            let now = filetime_now();
            db.transaction(
                |tx, outbox| {
                    let others = others.clone();
                    Box::pin(async move {
                        for other in others {
                            let school = school_corp(tx, other).await?;
                            set_corp(tx, outbox, other, school, 0, now).await?;
                        }
                        leave_inner(tx, outbox, me, now).await
                    })
                },
                |value, outbox| async move { dispatch_world(value, outbox, world) },
            ).await?;
            Ok(Value::None)
        }
    }
}

/// KeyVal(corpID, allianceID, title, title1..title16) for `char_id`: the names of the titles
/// they hold (None for the others).
async fn info_window_data(db: &Db, char_id: i64) -> CallResult {
    let Some(r) = db
        .query_opt(
            "SELECT ch.corporation_id, co.alliance_id, ch.corp_title, ch.corp_title_mask FROM game.characters ch
             LEFT JOIN game.corporations co ON co.corporation_id = ch.corporation_id WHERE ch.character_id = $1",
            &[&char_id],
        )
        .await
        .map_err(db_err)?
    else {
        return Ok(Value::None);
    };
    let (corp, mask): (i64, i64) = (r.get(0), r.get(3));
    let names = db.query("SELECT title_id, name FROM game.corp_titles WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?;
    let mut fields: Vec<(String, Value)> = vec![
        ("corpID".into(), Value::Int(corp)),
        ("allianceID".into(), r.get::<_, Option<i64>>(1).map(Value::Int).unwrap_or(Value::None)),
        ("title".into(), Value::unicode(r.get::<_, String>(2))),
    ];
    for bit in 0..TITLE_COUNT {
        let id = 1i64 << bit;
        let held = mask & id != 0;
        let name = names.iter().find(|n| n.get::<_, i64>(0) == id).map(|n| n.get::<_, String>(1)).unwrap_or_default();
        fields.push((format!("title{}", bit + 1), if held { Value::unicode(name) } else { Value::None }));
    }
    Ok(keyval(fields))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "corpRegistry" {
        return None;
    }
    // SetAccountKey(accountKey): the wallet division the member acts from.
    if call.method == "SetAccountKey" {
        let key = call.args.first().and_then(|v| v.as_int()).unwrap_or(ACCOUNT_CASH as i64);
        c.change_session(vec![("corpAccountKey", Value::Int(key))]);
        return Some(Ok(Value::None));
    }
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let (db, world) = (&c.db, &*c.world);
    let corp = bound_corp(c, call);
    Some(match call.method {
        "AddCorporation" => found(c, call).await,
        // GetSuggestedTickerNames(name) -> [KeyVal(tickerName)]
        "GetSuggestedTickerNames" => {
            let name = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            Ok(Value::list(super::alliances::suggested_names(&name).into_iter().map(|t| keyval([("tickerName", Value::str(t))]))))
        }
        // (canLeave, error, errorDetails)
        "CanLeaveCurrentCorporation" => match membership(db, me).await {
            Ok((my_corp, _)) => {
                let blocked = if !is_player_corp(my_corp) {
                    Some("CrpCantQuitDefaultCorporation")
                } else {
                    match (ceo_of(db, my_corp).await, members(db, my_corp, "AND $2::bigint[] IS NOT NULL", &[]).await) {
                        (Ok(Some(ceo)), Ok(list)) if ceo == me && list.len() > 1 => Some("CrpCantQuitCEO"),
                        (Err(e), _) | (_, Err(e)) => return Some(Err(e)),
                        _ => None,
                    }
                };
                Ok(Value::tuple([Value::Bool(blocked.is_none()), blocked.map(Value::str).unwrap_or(Value::None), Value::None]))
            }
            Err(e) => Err(e),
        },
        "CanBeKickedOut" => {
            let target = int(0).unwrap_or(0);
            ceo_of(db, corp).await.map(|ceo| Value::Bool(ceo != Some(target) || target == me))
        }
        "KickOutMember" | "KickOutMembers" => {
            let targets: Vec<i64> = match call.method {
                "KickOutMember" => int(0).into_iter().collect(),
                _ => value_id_list(call.args.first()),
            };
            let (my_corp, roles) = match membership(db, me).await {
                Ok(m) => m,
                Err(e) => return Some(Err(e)),
            };
            for target in &targets {
                if *target != me {
                    let (their_corp, _) = match membership(db, *target).await {
                        Ok(m) => m,
                        Err(e) => return Some(Err(e)),
                    };
                    if their_corp != my_corp || roles & ROLE_DIRECTOR == 0 || ceo_of(db, my_corp).await.ok().flatten() == Some(*target) {
                        return Some(Err(notify_error(messages::CANNOT_REMOVE_MEMBER)));
                    }
                }
                if let Err(e) = leave(db, world, *target).await {
                    return Some(Err(e));
                }
            }
            Ok(if call.method == "KickOutMembers" { Value::list([]) } else { Value::None })
        }
        // InsertApplication(corporationID, text)
        "InsertApplication" => {
            let target = int(0).unwrap_or(0);
            let text = call.args.get(1).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            if !is_player_corp(target) {
                return Some(Err(notify_error(messages::NPC_CORP_DOES_NOT_ACCEPT_APPLICATIONS)));
            }
            let recruiting: Option<bool> = match db.query_opt("SELECT is_recruiting FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&target]).await {
                Ok(r) => r.map(|r| r.get(0)),
                Err(e) => return Some(Err(db_err(e))),
            };
            if recruiting != Some(true) {
                return Some(Err(notify_error(messages::CORP_NOT_RECRUITING)));
            }
            db
                .query_one(
                    "INSERT INTO game.corp_applications (corporation_id, character_id, application_text, status, created) VALUES ($1, $2, $3, $4, $5) RETURNING application_id",
                    &[&target, &me, &text, &APP_APPLIED, &filetime_now()],
                )
                .await
                .map(|r| Value::Int(r.get(0)))
                .map_err(db_err)
        }
        // InsertInvitation(characterID)
        "InsertInvitation" => {
            let target = int(0).unwrap_or(0);
            match membership(db, me).await {
                Ok((my_corp, roles)) if is_player_corp(my_corp) && roles & (ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR) != 0 => db
                    .execute(
                        "INSERT INTO game.corp_applications (corporation_id, character_id, application_text, status, created) VALUES ($1, $2, '', $3, $4)",
                        &[&my_corp, &target, &APP_INVITED_BY_CORPORATION, &filetime_now()],
                    )
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::PERSONNEL_MANAGER_REQUIRED_TO_INVITE)),
                Err(e) => Err(e),
            }
        }
        "GetMyApplications" => applications(db, "character_id = $1", me, true).await,
        "GetApplications" => applications(db, "corporation_id = $1", corp, false).await,
        "GetMyOldApplications" => match db
            .query(&format!("{APPLICATION_SELECT} WHERE character_id = $1 AND NOT (status = ANY($2)) ORDER BY application_id DESC LIMIT $3"), &[&me, &OPEN_APPLICATION.to_vec(), &OLD_APPLICATION_LIMIT])
            .await
        {
            Ok(rows) => Ok(Value::list(rows.iter().map(application_row))),
            Err(e) => Err(db_err(e)),
        },
        "UpdateApplicationOffer" => update_application(c, call).await,
        // [row(ownerID, ownerName, typeID, gender)] of the members.
        "GetEveOwners" => match db
            .query("SELECT character_id, name, gender_id::bigint FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp])
            .await
        {
            Ok(rows) => {
                let header = DbRowDescriptor::new([("ownerID", DbType::I4), ("ownerName", DbType::WStr), ("typeID", DbType::I2), ("gender", DbType::I2)]);
                Ok(Value::list(rows.iter().map(|r| {
                    header.row(&[Value::Int(r.get(0)), Value::unicode(r.get::<_, String>(1)), Value::Int(TYPE_CHARACTER as i64), Value::Int(r.get(2))])
                })))
            }
            Err(e) => Err(db_err(e)),
        },
        // GetMembersPaged(page): everyone on the first page.
        "GetMembersPaged" => {
            if int(0).unwrap_or(1) > 1 {
                return Some(Ok(Value::list([])));
            }
            members(db, corp, "AND $2::bigint[] IS NOT NULL", &[]).await.map(|rows| Value::list(rows.iter().map(|r| member_row(r, corp))))
        }
        "GetMember" => {
            let id = int(0).unwrap_or(0);
            members(db, corp, "AND ch.character_id = ANY($2)", &[id]).await.map(|rows| rows.first().map(|r| member_row(r, corp)).unwrap_or(Value::None))
        }
        "GetMembersByIds" => {
            let ids = value_id_list(call.args.first());
            members(db, corp, "AND ch.character_id = ANY($2)", &ids).await.map(|rows| Value::list(rows.iter().map(|r| member_row(r, corp))))
        }
        "GetMemberIDsByQuery" => members(db, corp, "AND $2::bigint[] IS NOT NULL", &[]).await.map(|rows| Value::list(rows.iter().map(|r| Value::Int(r.get(0))))),
        "GetMemberTrackingInfo" | "GetMemberTrackingInfoSimple" => members(db, corp, "AND $2::bigint[] IS NOT NULL", &[]).await.map(|rows| {
            rowset(
                &["characterID", "roles", "grantableRoles", "baseID", "title", "startDateTime", "logonDateTime", "logoffDateTime", "locationID", "shipTypeID"],
                rows.iter()
                    .map(|r| {
                        let location = r.get::<_, Option<i64>>(3).or(r.get(4));
                        vec![
                            Value::Int(r.get(0)),
                            Value::Int(r.get(1)),
                            Value::Int(0),
                            Value::None,
                            Value::unicode(""),
                            Value::Int(r.get(2)),
                            Value::Int(r.get(5)),
                            Value::Int(r.get(5)),
                            location.map(Value::Int).unwrap_or(Value::None),
                            r.get::<_, Option<i32>>(6).map(|t| Value::Int(t as i64)).unwrap_or(Value::None),
                        ]
                    })
                    .collect(),
            )
        }),
        // [characterID] of the members who could take over as CEO (Corporation Management).
        "GetNumberOfPotentialCEOs" => potential_ceos(db, corp, me).await.map(|ids| Value::list(ids.into_iter().map(Value::Int))),
        "UpdateMember" => update_member(db, world, me, call.args).await.map(|_| Value::None),
        // UpdateMembers(Rowset of UpdateMember argument lines)
        "UpdateMembers" => {
            for line in rowset_lines(call.args.first()) {
                if let Err(e) = update_member(db, world, me, &line).await {
                    return Some(Err(e));
                }
            }
            Ok(Value::None)
        }
        "ExecuteActions" => execute_actions(db, world, me, call).await.map(|_| Value::None),
        "GetTitles" => titles(db, corp).await,
        // UpdateTitle(titleID, titleName, roles, grantableRoles, rolesAtHQ, grantableRolesAtHQ, rolesAtBase, ...)
        "UpdateTitle" => update_titles(db, world, me, corp, vec![call.args.to_vec()]).await.map(|_| Value::None),
        "UpdateTitles" => update_titles(db, world, me, corp, rowset_lines(call.args.first())).await.map(|_| Value::None),
        "DeleteTitle" => delete_title(db, world, me, corp, int(0).unwrap_or(0)).await.map(|_| Value::None),
        "ResignFromCEO" => resign_ceo(db, world, me, int(0)).await,
        "UpdateDivisionNames" => {
            let names: Vec<Option<String>> = (0..CORP_DIVISION_NAME_COUNT).map(|i| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned())).collect();
            match membership(db, me).await {
                Ok((my_corp, roles)) if my_corp == corp && roles & ROLE_DIRECTOR != 0 => db
                    .execute("UPDATE game.corporations SET division_names = $2 WHERE corporation_id = $1", &[&corp, &names])
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_DIVISION_NAMES)),
                Err(e) => Err(e),
            }
        }
        // The member limit follows the CEO's skills; it isn't enforced, so nothing to recompute.
        "UpdateCorporationAbilities" => Ok(Value::Bool(true)),
        // KeyVal(corpID, allianceID, title, title1..title16): a member's titles, for show info.
        "GetInfoWindowDataForChar" => info_window_data(db, int(0).unwrap_or(me)).await,
        // Shares aren't modelled.
        "GetMemberIDsWithMoreThanAvgShares" => Ok(Value::list([])),
        "PayoutDividend" | "MoveCompanyShares" | "MovePrivateShares" => Err(notify_error(messages::CORP_SHARES_UNAVAILABLE)),
        // UpdateCorporation(description, url, taxRate, isRecruiting, lpTaxRate)
        "UpdateCorporation" => {
            let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let tax = call.args.get(2).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            let recruiting = call.args.get(3).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0));
            let lp_tax = call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            match membership(db, me).await {
                Ok((my_corp, roles)) if my_corp == corp && roles & ROLE_DIRECTOR != 0 => db
                    .execute(
                        "UPDATE game.corporations SET description = $2, url = $3, tax_rate = $4, is_recruiting = $5, lp_tax_rate = $6 WHERE corporation_id = $1",
                        &[&corp, &text(0), &text(1), &tax, &recruiting, &lp_tax],
                    )
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error(messages::DIRECTOR_REQUIRED_TO_EDIT_CORP_INFO)),
                Err(e) => Err(e),
            }
        }
        "UpdateLogo" => {
            let s = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
            db
                .execute(
                    "UPDATE game.corporations SET shape1 = $2, shape2 = $3, shape3 = $4, color1 = $5, color2 = $6, color3 = $7, typeface = $8
                     WHERE corporation_id = $1 AND ceo_id = $9",
                    &[&corp, &s(0), &s(1), &s(2), &s(3), &s(4), &s(5), &s(6), &me],
                )
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        "GetCorpWelcomeMail" => match db.query_opt("SELECT welcome_mail FROM game.corporations WHERE corporation_id = $1", &[&corp]).await {
            Ok(r) => Ok(Value::unicode(r.map(|r| r.get::<_, String>(0)).unwrap_or_default())),
            Err(e) => Err(db_err(e)),
        },
        "SetCorpWelcomeMail" => {
            let text = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            db
                .execute("UPDATE game.corporations SET welcome_mail = $2 WHERE corporation_id = $1 AND ceo_id = $3", &[&corp, &text, &me])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        _ => return None,
    })
}
