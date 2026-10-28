//! Player corporations (corpRegistry, bound per corporation; eve/client/script/ui/services/corporation).
//!
//! A character with Corporation Management can found a corporation (game.corporations, IDs
//! from 98,000,000 like the live server's) for appConst.corporationStartupCost while docked,
//! and becomes its CEO with every role. Others join by application (or invitation) that the
//! corporation's personnel managers, or the character, accept; members leave, or directors
//! kick them, back to their school's NPC corporation. Membership changes reach an online
//! pilot as a session change (corpid, corprole...), which also moves them to the new corp
//! chat channel. A member's roles are one mask (game.characters.corp_roles) used for all
//! locations. Shares, titles, wallets divisions, hangars, offices and alliances aren't
//! supported.

use super::{keyval, rowset, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::db::Db;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::info;

/// appConst.corporationStartupCost
const STARTUP_COST: f64 = 1_599_800.0;
const SKILL_CORPORATION_MANAGEMENT: i32 = 3363;
const TYPE_CHARACTER: i64 = 1373;
// appConst.corpRole*
const ROLE_DIRECTOR: i64 = 1;
const ROLE_PERSONNEL_MANAGER: i64 = 128;
/// The CEO holds every role.
const CEO_ROLES: i64 = i64::MAX;
// appConst.crpApplication*
const APP_APPLIED: i32 = 0;
const APP_RENEGOTIATED_BY_CHARACTER: i32 = 1;
const APP_ACCEPTED_BY_CHARACTER: i32 = 2;
const APP_REJECTED_BY_CHARACTER: i32 = 3;
const APP_RENEGOTIATED_BY_CORPORATION: i32 = 5;
const APP_ACCEPTED_BY_CORPORATION: i32 = 6;
const APP_WITHDRAWN_BY_CHARACTER: i32 = 7;
const APP_INVITED_BY_CORPORATION: i32 = 8;
const OPEN_APPLICATION: [i32; 4] = [APP_APPLIED, APP_RENEGOTIATED_BY_CHARACTER, APP_RENEGOTIATED_BY_CORPORATION, APP_INVITED_BY_CORPORATION];
/// Player corporation IDs (idCheckers.IsPlayerCorporation).
const FIRST_PLAYER_CORP: i64 = 98_000_000;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

fn notify_error(text: &str) -> CallError {
    CallError::User { msg: "CustomNotify".into(), dict: vec![("notify".into(), Value::unicode(text))] }
}

pub fn is_player_corp(id: i64) -> bool {
    id >= FIRST_PLAYER_CORP
}

/// The corporation `char_id` is in, and their role mask.
async fn membership(db: &Db, char_id: i64) -> Result<(i64, i64), CallError> {
    let r = db
        .client
        .query_one("SELECT corporation_id, corp_roles FROM game.characters WHERE character_id = $1", &[&char_id])
        .await
        .map_err(db_err)?;
    Ok((r.get(0), r.get(1)))
}

async fn ceo_of(db: &Db, corp: i64) -> Result<Option<i64>, CallError> {
    Ok(db
        .client
        .query_opt("SELECT ceo_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await
        .map_err(db_err)?
        .and_then(|r| r.get(0)))
}

/// Move a character into `corp` with `roles`, recording the employment and updating their
/// session if they're online.
async fn set_corp(db: &Db, world: &World, char_id: i64, corp: i64, roles: i64) -> Result<(), CallError> {
    let hq: Option<i64> = if is_player_corp(corp) {
        db.client.query_opt("SELECT station_id FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?.map(|r| r.get(0))
    } else {
        db.client
            .query_opt("SELECT station_id::bigint FROM sde.crp_npc_corporations WHERE corporation_id = $1::bigint", &[&corp])
            .await
            .map_err(db_err)?
            .and_then(|r| r.get(0))
    };
    db.client
        .execute("UPDATE game.characters SET corporation_id = $2, corp_roles = $3 WHERE character_id = $1", &[&char_id, &corp, &roles])
        .await
        .map_err(db_err)?;
    db.client
        .execute("INSERT INTO game.employment_history (char_id, corporation_id, start_date) VALUES ($1, $2, $3)", &[&char_id, &corp, &filetime_now()])
        .await
        .map_err(db_err)?;
    let pair = |k: &str, v: Value| Value::tuple([Value::str(k), v]);
    world.notify_pilot(
        char_id,
        "__session",
        vec![
            pair("corpid", Value::Int(corp)),
            pair("corprole", Value::Int(roles)),
            pair("rolesAtAll", Value::Int(roles)),
            pair("rolesAtBase", Value::Int(roles)),
            pair("rolesAtHQ", Value::Int(roles)),
            pair("rolesAtOther", Value::Int(roles)),
            pair("hqID", hq.map(Value::Int).unwrap_or(Value::None)),
        ],
    );
    info!("corporations: {char_id} is now in {corp}");
    Ok(())
}

/// The NPC corporation of the character's school.
async fn school_corp(db: &Db, char_id: i64) -> Result<i64, CallError> {
    let r = db
        .client
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
    let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.trim().to_string()).unwrap_or_default();
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
    let (name, ticker, description, url) = (text(0), text(1).to_uppercase(), text(2), text(3));
    let tax = call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
    let Some(station) = c.session.int("stationid") else {
        return Err(notify_error("你必须停靠在空间站内才能创建军团。"));
    };
    if !(4..=50).contains(&name.chars().count()) {
        return Err(notify_error("军团名称长度必须为 4 到 50 个字符。"));
    }
    if !(2..=5).contains(&ticker.chars().count()) || !ticker.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(notify_error("军团代号必须为 2 到 5 个字母、数字、点或短横线。"));
    }
    let skill: i32 = db
        .client
        .query_opt("SELECT level FROM game.skills WHERE character_id = $1 AND type_id = $2", &[&me, &SKILL_CORPORATION_MANAGEMENT])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(0);
    if skill < 1 {
        return Err(notify_error("创建军团需要军团管理技能。"));
    }
    let (current, _) = membership(db, me).await?;
    if is_player_corp(current) && ceo_of(db, current).await? == Some(me) {
        return Err(notify_error("你已是 CEO，请先辞职或关闭你的军团。"));
    }
    let taken: bool = db
        .client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM game.corporations WHERE NOT deleted AND (lower(name) = lower($1) OR upper(ticker) = $2))
                 OR EXISTS (SELECT 1 FROM sde.crp_npc_corporations WHERE lower(name) = lower($1) OR upper(ticker) = $2)",
            &[&name, &ticker],
        )
        .await
        .map_err(db_err)?
        .get(0);
    if taken {
        return Err(notify_error("该军团名称或代号已被占用。"));
    }
    let _trading = super::market::lock().await;
    super::market::debit(db, world, me, STARTUP_COST).await?;
    let id: i64 = db
        .client
        .query_one(
            "INSERT INTO game.corporations (name, ticker, description, url, tax_rate, ceo_id, creator_id, station_id, is_recruiting, friendly_fire,
                                            shape1, shape2, shape3, color1, color2, color3, typeface, created)
             VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING corporation_id",
            &[
                &name,
                &ticker,
                &description,
                &url,
                &tax,
                &me,
                &station,
                &(call.args.get(12).and_then(|v| v.as_int()).unwrap_or(1) != 0),
                &matches!(call.args.get(13), Some(Value::Bool(true))),
                &int(5),
                &int(6),
                &int(7),
                &int(8),
                &int(9),
                &int(10),
                &int(11),
                &filetime_now(),
            ],
        )
        .await
        .map_err(db_err)?
        .get(0);
    set_corp(db, world, me, id, CEO_ROLES).await?;
    info!("corporations: {me} founded {id} [{ticker}] {name}");
    Ok(Value::Int(id))
}

/// Leave (or be kicked from) a player corporation, back to the school's NPC corporation.
/// A CEO can only leave as the last member, which closes the corporation.
async fn leave(db: &Db, world: &World, char_id: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, char_id).await?;
    if !is_player_corp(corp) {
        return Err(notify_error("不能用这种方式退出 NPC 军团。"));
    }
    if ceo_of(db, corp).await? == Some(char_id) {
        let others: i64 = db
            .client
            .query_one("SELECT count(*) FROM game.characters WHERE corporation_id = $1 AND character_id <> $2 AND NOT deleted", &[&corp, &char_id])
            .await
            .map_err(db_err)?
            .get(0);
        if others > 0 {
            return Err(notify_error("军团还有其他成员时，CEO 不能退出。"));
        }
        db.client.execute("UPDATE game.corporations SET deleted = true, ceo_id = NULL WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?;
        info!("corporations: {corp} closed");
    }
    let school = school_corp(db, char_id).await?;
    set_corp(db, world, char_id, school, 0).await
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
        .client
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
    let Some(r) = db.client.query_opt(&format!("{APPLICATION_SELECT} WHERE application_id = $1"), &[&app_id]).await.map_err(db_err)? else {
        return Err(notify_error("该申请已不存在。"));
    };
    let (corp, applicant, current): (i64, i64, i32) = (r.get(1), r.get(2), r.get(4));
    if !OPEN_APPLICATION.contains(&current) {
        return Err(notify_error("该申请已处理完毕。"));
    }
    let by_character = matches!(status, APP_ACCEPTED_BY_CHARACTER | APP_REJECTED_BY_CHARACTER | APP_WITHDRAWN_BY_CHARACTER | APP_RENEGOTIATED_BY_CHARACTER);
    if by_character && applicant != me {
        return Err(notify_error("这不是你的申请。"));
    }
    if !by_character {
        let (my_corp, roles) = membership(db, me).await?;
        if my_corp != corp || roles & (ROLE_PERSONNEL_MANAGER | ROLE_DIRECTOR) == 0 {
            return Err(notify_error("只有军团的人事经理才能处理申请。"));
        }
    }
    db.client
        .execute("UPDATE game.corp_applications SET status = $2, custom_message = $3 WHERE application_id = $1", &[&app_id, &status, &message])
        .await
        .map_err(db_err)?;
    // An application the corporation accepts, or an invitation the character accepts, is a new member.
    let joins = (status == APP_ACCEPTED_BY_CORPORATION && current != APP_INVITED_BY_CORPORATION)
        || (status == APP_ACCEPTED_BY_CHARACTER && current == APP_INVITED_BY_CORPORATION);
    if joins {
        let (old, _) = membership(db, applicant).await?;
        if is_player_corp(old) && ceo_of(db, old).await? == Some(applicant) {
            return Err(notify_error("CEO 必须先离开自己的军团才能加入其他军团。"));
        }
        set_corp(db, world, applicant, corp, 0).await?;
        db.client
            .execute("UPDATE game.corp_applications SET status = $3 WHERE character_id = $1 AND application_id <> $2 AND status = ANY($4)", &[&applicant, &app_id, &APP_WITHDRAWN_BY_CHARACTER, &OPEN_APPLICATION.to_vec()])
            .await
            .map_err(db_err)?;
    }
    Ok(Value::None)
}

/// A member row (bco_members, corp_ui_member_*).
fn member_row(r: &tokio_postgres::Row, corp: i64) -> Value {
    let roles: i64 = r.get(1);
    keyval([
        ("characterID", Value::Int(r.get(0))),
        ("corporationID", Value::Int(corp)),
        ("roles", Value::Int(roles)),
        ("grantableRoles", Value::Int(if roles & ROLE_DIRECTOR != 0 { roles } else { 0 })),
        ("rolesAtHQ", Value::Int(roles)),
        ("grantableRolesAtHQ", Value::Int(0)),
        ("rolesAtBase", Value::Int(roles)),
        ("grantableRolesAtBase", Value::Int(0)),
        ("rolesAtOther", Value::Int(roles)),
        ("grantableRolesAtOther", Value::Int(0)),
        ("baseID", Value::None),
        ("titleMask", Value::Int(0)),
        ("title", Value::unicode("")),
        ("startDateTime", Value::Int(r.get(2))),
        ("divisionID", Value::Int(0)),
        ("squadronID", Value::Int(0)),
        ("blockRoles", Value::None),
        ("accountKey", Value::Int(1000)),
    ])
}

const MEMBER_SELECT: &str = "SELECT ch.character_id, ch.corp_roles,
        COALESCE((SELECT max(e.start_date) FROM game.employment_history e WHERE e.char_id = ch.character_id AND e.corporation_id = ch.corporation_id),
                 (extract(epoch FROM ch.created_at) * 10000000)::bigint + 116444736000000000),
        ch.station_id, ch.solar_system_id, (extract(epoch FROM ch.logoff_at) * 10000000)::bigint + 116444736000000000,
        (SELECT type_id FROM game.items WHERE item_id = ch.ship_id)
    FROM game.characters ch";

async fn members(db: &Db, corp: i64, extra: &str, ids: &[i64]) -> Result<Vec<tokio_postgres::Row>, CallError> {
    db.client
        .query(&format!("{MEMBER_SELECT} WHERE ch.corporation_id = $1 AND NOT ch.deleted {extra} ORDER BY ch.character_id"), &[&corp, &ids.to_vec()])
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
        .client
        .query(
            "SELECT corporation_id, start_date FROM game.employment_history WHERE char_id = $1
             UNION ALL
             SELECT ch.corporation_id, (extract(epoch FROM ch.created_at) * 10000000)::bigint + 116444736000000000
             FROM game.characters ch WHERE ch.character_id = $1 AND NOT EXISTS (SELECT 1 FROM game.employment_history WHERE char_id = $1)
             ORDER BY 2 DESC",
            &[&char_id],
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
        .client
        .query_opt(
            "SELECT name, description, ticker, url, tax_rate, lp_tax_rate, creator_id, ceo_id, station_id, is_recruiting, friendly_fire,
                    shape1::bigint, shape2::bigint, shape3::bigint, color1::bigint, color2::bigint, color3::bigint, typeface::bigint, created, deleted,
                    (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = $1 AND NOT ch.deleted)
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
        Value::Int(2), // corporationType: player
        Value::Bool(true),
        Value::Bool(true),
        Value::Int(r.get(6)),
        opt(7),
        Value::Int(r.get(8)),
        Value::None,
        Value::None,
        Value::None,
        Value::Int(1000),
        Value::Int(r.get(20)),
        Value::Int(-1),
        Value::None,
        Value::None,
    ];
    line.extend((11..18).map(opt));
    line.extend(std::iter::repeat_n(Value::None, 14)); // division names
    line.extend([Value::Bool(r.get(19)), Value::Bool(r.get(9)), Value::Bool(r.get(10)), Value::None, Value::Int(r.get(18))]);
    Ok(Some(line))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "corpRegistry" {
        return None;
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
            let initials: String = name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_ascii_alphanumeric())).collect::<String>().to_uppercase();
            let compact: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase();
            let mut out: Vec<String> = vec![];
            for t in [initials.chars().take(5).collect::<String>(), compact.chars().take(5).collect(), compact.chars().take(4).collect(), compact.chars().take(3).collect()] {
                if t.len() >= 2 && !out.contains(&t) {
                    out.push(t);
                }
            }
            Ok(Value::list(out.into_iter().map(|t| keyval([("tickerName", Value::str(t))]))))
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
                _ => call.args.first().and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).collect(),
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
                        return Some(Err(notify_error("你无法移除该成员。")));
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
                return Some(Err(notify_error("NPC 军团不接受申请。")));
            }
            let recruiting: Option<bool> = match db.client.query_opt("SELECT is_recruiting FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&target]).await {
                Ok(r) => r.map(|r| r.get(0)),
                Err(e) => return Some(Err(db_err(e))),
            };
            if recruiting != Some(true) {
                return Some(Err(notify_error("该军团目前不接受申请。")));
            }
            db.client
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
                    .client
                    .execute(
                        "INSERT INTO game.corp_applications (corporation_id, character_id, application_text, status, created) VALUES ($1, $2, '', $3, $4)",
                        &[&my_corp, &target, &APP_INVITED_BY_CORPORATION, &filetime_now()],
                    )
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error("只有人事经理才能发出邀请。")),
                Err(e) => Err(e),
            }
        }
        "GetMyApplications" => applications(db, "character_id = $1", me, true).await,
        "GetApplications" => applications(db, "corporation_id = $1", corp, false).await,
        "GetMyOldApplications" => match db
            .client
            .query(&format!("{APPLICATION_SELECT} WHERE character_id = $1 AND NOT (status = ANY($2)) ORDER BY application_id DESC LIMIT 100"), &[&me, &OPEN_APPLICATION.to_vec()])
            .await
        {
            Ok(rows) => Ok(Value::list(rows.iter().map(application_row))),
            Err(e) => Err(db_err(e)),
        },
        "UpdateApplicationOffer" => update_application(c, call).await,
        // [row(ownerID, ownerName, typeID, gender)] of the members.
        "GetEveOwners" => match db
            .client
            .query("SELECT character_id, name, gender_id::bigint FROM game.characters WHERE corporation_id = $1 AND NOT deleted", &[&corp])
            .await
        {
            Ok(rows) => {
                let header = DbRowDescriptor::new([("ownerID", DbType::I4), ("ownerName", DbType::WStr), ("typeID", DbType::I2), ("gender", DbType::I2)]);
                Ok(Value::list(rows.iter().map(|r| {
                    header.row(&[Value::Int(r.get(0)), Value::unicode(r.get::<_, String>(1)), Value::Int(TYPE_CHARACTER), Value::Int(r.get(2))])
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
            let ids: Vec<i64> = call.args.first().and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).collect();
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
        "GetNumberOfPotentialCEOs" => members(db, corp, "AND $2::bigint[] IS NOT NULL", &[]).await.map(|rows| Value::Int(rows.len() as i64 - 1)),
        // UpdateMember(characterID, title, divisionID, squadronID, roles, grantableRoles, rolesAtHQ, ...)
        "UpdateMember" => {
            let target = int(0).unwrap_or(0);
            let roles = int(4).unwrap_or(0);
            match (membership(db, me).await, membership(db, target).await, ceo_of(db, corp).await) {
                (Ok((my_corp, my_roles)), Ok((their_corp, _)), Ok(ceo)) => {
                    if my_corp != their_corp || my_roles & ROLE_DIRECTOR == 0 || ceo == Some(target) {
                        Err(notify_error("你无法修改该成员的职权。"))
                    } else {
                        match db.client.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&target, &roles]).await {
                            Ok(_) => {
                                let pair = |k: &str| Value::tuple([Value::str(k), Value::Int(roles)]);
                                world.notify_pilot(target, "__session", vec![pair("corprole"), pair("rolesAtAll"), pair("rolesAtBase"), pair("rolesAtHQ"), pair("rolesAtOther")]);
                                Ok(Value::None)
                            }
                            Err(e) => Err(db_err(e)),
                        }
                    }
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            }
        }
        // UpdateCorporation(description, url, taxRate, isRecruiting, lpTaxRate)
        "UpdateCorporation" => {
            let text = |i: usize| call.args.get(i).and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            let tax = call.args.get(2).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            let recruiting = call.args.get(3).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0));
            let lp_tax = call.args.get(4).and_then(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
            match membership(db, me).await {
                Ok((my_corp, roles)) if my_corp == corp && roles & ROLE_DIRECTOR != 0 => db
                    .client
                    .execute(
                        "UPDATE game.corporations SET description = $2, url = $3, tax_rate = $4, is_recruiting = $5, lp_tax_rate = $6 WHERE corporation_id = $1",
                        &[&corp, &text(0), &text(1), &tax, &recruiting, &lp_tax],
                    )
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err),
                Ok(_) => Err(notify_error("只有董事才能修改军团信息。")),
                Err(e) => Err(e),
            }
        }
        "UpdateLogo" => {
            let s = |i: usize| call.args.get(i).and_then(|v| v.as_int()).map(|v| v as i32);
            db.client
                .execute(
                    "UPDATE game.corporations SET shape1 = $2, shape2 = $3, shape3 = $4, color1 = $5, color2 = $6, color3 = $7, typeface = $8
                     WHERE corporation_id = $1 AND ceo_id = $9",
                    &[&corp, &s(0), &s(1), &s(2), &s(3), &s(4), &s(5), &s(6), &me],
                )
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        "GetCorpWelcomeMail" => match db.client.query_opt("SELECT welcome_mail FROM game.corporations WHERE corporation_id = $1", &[&corp]).await {
            Ok(r) => Ok(Value::unicode(r.map(|r| r.get::<_, String>(0)).unwrap_or_default())),
            Err(e) => Err(db_err(e)),
        },
        "SetCorpWelcomeMail" => {
            let text = call.args.first().and_then(|v| v.as_text()).map(|t| t.into_owned()).unwrap_or_default();
            db.client
                .execute("UPDATE game.corporations SET welcome_mail = $2 WHERE corporation_id = $1 AND ceo_id = $3", &[&corp, &text, &me])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        _ => return None,
    })
}
