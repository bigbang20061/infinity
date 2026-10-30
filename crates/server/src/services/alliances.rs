//! Alliances (allianceRegistry, bound per alliance; eve/client/script/ui/services/alliances).
//!
//! A player corporation's CEO founds an alliance (game.alliances, IDs from 99,000,000 like
//! the live server's) for appConst.allianceCreationCost from the corporation's master wallet;
//! the corporation becomes its executor. Other corporations'
//! CEOs apply, and the executor's directors accept (the corporation joins at once) or reject.
//! A member corporation leaves, or the executor's directors remove it; when the executor
//! leaves, the largest remaining member takes over, and the last member leaving closes the
//! alliance. Members back an executor (DeclareExecutorSupport); a candidate backed by
//! corporations holding more than half the alliance's pilots takes over. Membership changes
//! reach every online member as a session change (allianceid) and as the client's
//! OnAlliance*Changed row updates. Wars, bills, sovereignty, capital systems and prime time
//! aren't supported.

use super::corps::{ceo_of, is_player_corp, membership};
use super::{crowset, db_err, keyval, notify_error, trimmed_arg_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, ORG_NAME_MAX_LENGTH, ORG_NAME_MIN_LENGTH, ORG_SHORT_NAME_MAX_LENGTH, ORG_SHORT_NAME_MIN_LENGTH, ROLE_DIRECTOR};
use crate::db::Db;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use tracing::info;

/// appConst.allianceCreationCost
const CREATION_COST: f64 = 1_000_000_000.0;
/// Client appConst.allianceApplicationNew.
const APP_NEW: i32 = 1;
/// Client appConst.allianceApplicationAccepted.
const APP_ACCEPTED: i32 = 2;
/// Client appConst.allianceApplicationEffective.
const APP_EFFECTIVE: i32 = 3;
/// Client appConst.allianceApplicationRejected.
const APP_REJECTED: i32 = 4;
/// Server policy: default number of ranked alliances returned.
const DEFAULT_RANKED_LIMIT: i64 = 100;
/// Server policy: maximum number of ranked alliances returned.
const MAX_RANKED_LIMIT: i64 = 1000;
/// Server majority calculation: twice the backing must exceed the total.
const MAJORITY_DENOMINATOR: i64 = 2;
/// Server suggestion algorithm: maximum ticker or short-name length.
const SUGGESTION_MAX_LENGTH: usize = 5;
/// Server suggestion algorithm: fallback compact-name lengths.
const SUGGESTION_FALLBACK_LENGTHS: [usize; 2] = [4, 3];
/// Server suggestion algorithm: minimum returned name length.
const SUGGESTION_MIN_LENGTH: usize = 2;

/// `dbutil.CIndexedRowset(header, column)` filled with `rows` keyed by their `column` value.
fn indexed_rowset(header: &DbRowDescriptor, column: &str, rows: Vec<(i64, Value)>) -> Value {
    Value::Reduce {
        data: Box::new(Value::tuple([
            Value::global("carbon.common.script.sys.crowset.CIndexedRowset"),
            Value::tuple([header.to_value(), Value::str(column)]),
        ])),
        list: vec![],
        dict: rows.into_iter().map(|(k, row)| (Value::Int(k), row)).collect(),
    }
}

/// An OnAlliance*Changed `change`: {column: (old, new)}; a missing side is an added or
/// removed row.
fn change(columns: &[&str], old: Option<&[Value]>, new: Option<&[Value]>) -> Value {
    let side = |v: Option<&[Value]>, i: usize| v.and_then(|v| v.get(i)).cloned().unwrap_or(Value::None);
    Value::dict(columns.iter().enumerate().map(|(i, c)| (*c, Value::tuple([side(old, i), side(new, i)]))))
}

// ---------------------------------------------------------------- rows

const ALLIANCE_COLUMNS: &[&str] = &[
    "allianceID",
    "allianceName",
    "shortName",
    "executorCorpID",
    "creatorCorpID",
    "creatorCharID",
    "warFactionID",
    "description",
    "url",
    "startDate",
    "memberCount",
    "corporationCount",
    "dictatorial",
    "allowWar",
    "currentCapital",
    "deleted",
];

fn alliance_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("allianceID", DbType::I4),
        ("allianceName", DbType::WStr),
        ("shortName", DbType::WStr),
        ("executorCorpID", DbType::I4),
        ("creatorCorpID", DbType::I4),
        ("creatorCharID", DbType::I4),
        ("warFactionID", DbType::I4),
        ("description", DbType::WStr),
        ("url", DbType::WStr),
        ("startDate", DbType::FileTime),
        ("memberCount", DbType::I4),
        ("corporationCount", DbType::I4),
        ("dictatorial", DbType::Bool),
        ("allowWar", DbType::Bool),
        ("currentCapital", DbType::I4),
        ("deleted", DbType::Bool),
    ])
}

const ALLIANCE_SELECT: &str = "SELECT a.alliance_id, a.name, a.short_name, a.executor_corp_id, a.creator_corp_id, a.creator_char_id,
        a.description, a.url, a.created, a.dictatorial, a.deleted,
        (SELECT count(*) FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
          WHERE co.alliance_id = a.alliance_id AND NOT ch.deleted),
        (SELECT count(*) FROM game.corporations co WHERE co.alliance_id = a.alliance_id AND NOT co.deleted)
    FROM game.alliances a";

fn alliance_line(r: &tokio_postgres::Row) -> Vec<Value> {
    let opt = |i: usize| r.get::<_, Option<i64>>(i).map(Value::Int).unwrap_or(Value::None);
    vec![
        Value::Int(r.get(0)),
        Value::unicode(r.get::<_, String>(1)),
        Value::unicode(r.get::<_, String>(2)),
        opt(3),
        Value::Int(r.get(4)),
        Value::Int(r.get(5)),
        Value::None,
        Value::unicode(r.get::<_, String>(6)),
        Value::unicode(r.get::<_, String>(7)),
        Value::Int(r.get(8)),
        Value::Int(r.get(11)),
        Value::Int(r.get(12)),
        Value::Bool(r.get(9)),
        Value::Bool(true),
        Value::None,
        Value::Bool(r.get(10)),
    ]
}

async fn alliance_line_of(db: &Db, alliance: i64) -> Result<Option<Vec<Value>>, CallError> {
    Ok(db.query_opt(&format!("{ALLIANCE_SELECT} WHERE a.alliance_id = $1"), &[&alliance]).await.map_err(db_err)?.map(|r| alliance_line(&r)))
}

async fn alliance_row(db: &Db, alliance: i64) -> CallResult {
    Ok(alliance_line_of(db, alliance).await?.map(|line| alliance_header().row(&line)).unwrap_or(Value::None))
}

const MEMBER_COLUMNS: &[&str] = &["corporationID", "allianceID", "chosenExecutorID", "startDate", "deleted"];

fn member_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("corporationID", DbType::I4),
        ("allianceID", DbType::I4),
        ("chosenExecutorID", DbType::I4),
        ("startDate", DbType::FileTime),
        ("deleted", DbType::Bool),
    ])
}

/// (corporationID, line) of each member corporation.
async fn member_lines(db: &Db, alliance: i64) -> Result<Vec<(i64, Vec<Value>)>, CallError> {
    let rows = db
        .query(
            "SELECT corporation_id, chosen_executor_id, COALESCE(alliance_joined, created) FROM game.corporations
             WHERE alliance_id = $1 AND NOT deleted ORDER BY corporation_id",
            &[&alliance],
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let corp: i64 = r.get(0);
            let line = vec![
                Value::Int(corp),
                Value::Int(alliance),
                r.get::<_, Option<i64>>(1).map(Value::Int).unwrap_or(Value::None),
                Value::Int(r.get(2)),
                Value::Bool(false),
            ];
            (corp, line)
        })
        .collect())
}

const APPLICATION_COLUMNS: &[&str] = &["allianceID", "corporationID", "applicationText", "applicationDateTime", "state"];

fn application_header() -> DbRowDescriptor {
    DbRowDescriptor::new([
        ("allianceID", DbType::I4),
        ("corporationID", DbType::I4),
        ("applicationText", DbType::WStr),
        ("applicationDateTime", DbType::FileTime),
        ("state", DbType::I4),
    ])
}

const APPLICATION_SELECT: &str = "SELECT alliance_id, corporation_id, application_text, created, state FROM game.alliance_applications";

fn application_line(r: &tokio_postgres::Row) -> Vec<Value> {
    vec![
        Value::Int(r.get(0)),
        Value::Int(r.get(1)),
        Value::unicode(r.get::<_, String>(2)),
        Value::Int(r.get(3)),
        Value::Int(r.get::<_, i32>(4) as i64),
    ]
}

async fn application_line_of(db: &Db, alliance: i64, corp: i64) -> Result<Option<Vec<Value>>, CallError> {
    Ok(db
        .query_opt(&format!("{APPLICATION_SELECT} WHERE alliance_id = $1 AND corporation_id = $2"), &[&alliance, &corp])
        .await
        .map_err(db_err)?
        .map(|r| application_line(&r)))
}

const RELATIONSHIP_COLUMNS: &[&str] = &["toID", "relationship"];

fn relationship_header() -> DbRowDescriptor {
    DbRowDescriptor::new([("toID", DbType::I4), ("relationship", DbType::I4)])
}

// ---------------------------------------------------------------- notifications

/// Every non-deleted character of the corporations in `alliance`, or of `corp` alone.
async fn pilots(db: &Db, alliance: Option<i64>, corp: Option<i64>) -> Result<Vec<i64>, CallError> {
    let rows = db
        .query(
            "SELECT ch.character_id FROM game.characters ch JOIN game.corporations co ON co.corporation_id = ch.corporation_id
             WHERE NOT ch.deleted AND (co.alliance_id = $1 OR co.corporation_id = $2)",
            &[&alliance, &corp],
        )
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Tell the alliance's online members (and `also_corp`'s) about a change.
async fn tell(db: &Db, world: &World, alliance: i64, also_corp: Option<i64>, event: &str, args: Vec<Value>) -> Result<(), CallError> {
    for char_id in pilots(db, Some(alliance), also_corp).await? {
        world.notify_pilot(char_id, event, args.clone());
    }
    Ok(())
}

async fn alliance_changed(db: &Db, world: &World, alliance: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = alliance_line_of(db, alliance).await?;
    let ch = change(ALLIANCE_COLUMNS, old.as_deref(), new.as_deref());
    tell(db, world, alliance, None, "OnAllianceChanged", vec![Value::Int(alliance), ch]).await
}

async fn application_changed(db: &Db, world: &World, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = application_line_of(db, alliance, corp).await?;
    let ch = change(APPLICATION_COLUMNS, old.as_deref(), new.as_deref());
    tell(db, world, alliance, Some(corp), "OnAllianceApplicationChanged", vec![Value::Int(alliance), Value::Int(corp), ch]).await
}

async fn member_changed(db: &Db, world: &World, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let new = member_lines(db, alliance).await?.into_iter().find(|(c, _)| *c == corp).map(|(_, l)| l);
    let ch = change(MEMBER_COLUMNS, old.as_deref(), new.as_deref());
    tell(db, world, alliance, Some(corp), "OnAllianceMemberChanged", vec![Value::Int(alliance), Value::Int(corp), ch]).await
}

/// Put `corp` in `alliance` (or none): history, and every member's session.
async fn set_alliance(db: &Db, world: &World, corp: i64, alliance: Option<i64>) -> Result<(), CallError> {
    let now = filetime_now();
    db
        .execute(
            "UPDATE game.corporations SET alliance_id = $2, alliance_joined = $3, chosen_executor_id = NULL WHERE corporation_id = $1",
            &[&corp, &alliance, &alliance.map(|_| now)],
        )
        .await
        .map_err(db_err)?;
    db
        .execute("INSERT INTO game.alliance_history (corporation_id, alliance_id, start_date) VALUES ($1, $2, $3)", &[&corp, &alliance, &now])
        .await
        .map_err(db_err)?;
    let value = alliance.map(Value::Int).unwrap_or(Value::None);
    for char_id in pilots(db, None, Some(corp)).await? {
        world.notify_pilot(char_id, "__session", vec![Value::tuple([Value::str("allianceid"), value.clone()])]);
    }
    info!("alliances: corporation {corp} is now in {alliance:?}");
    Ok(())
}

// ---------------------------------------------------------------- membership

/// The CEO of a player corporation that isn't in an alliance yet.
async fn unallied_ceo_corp(db: &Db, me: i64) -> Result<i64, CallError> {
    let (corp, _) = membership(db, me).await?;
    if !is_player_corp(corp) || ceo_of(db, corp).await? != Some(me) {
        return Err(notify_error("只有玩家军团的 CEO 才能这样做。"));
    }
    let allied: Option<i64> = db.query_one("SELECT alliance_id FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?.get(0);
    if allied.is_some() {
        return Err(notify_error("你的军团已经在一个联盟中。"));
    }
    Ok(corp)
}

/// `CreateAlliance(allianceName, shortName, description, url)` on the founder's corpRegistry.
async fn create(c: &Client, call: &Call<'_>) -> CallResult {
    let (db, world) = (&c.db, &*c.world);
    let me = c.session.int("charid").unwrap_or(0);
    let (name, short_name, description, url) = (trimmed_arg_text(call, 0), trimmed_arg_text(call, 1).to_uppercase(), trimmed_arg_text(call, 2), trimmed_arg_text(call, 3));
    let corp = unallied_ceo_corp(db, me).await?;
    if !(ORG_NAME_MIN_LENGTH..=ORG_NAME_MAX_LENGTH).contains(&name.chars().count()) {
        return Err(notify_error("联盟名称长度必须为 4 到 50 个字符。"));
    }
    if !(ORG_SHORT_NAME_MIN_LENGTH..=ORG_SHORT_NAME_MAX_LENGTH).contains(&short_name.chars().count()) || !short_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return Err(notify_error("联盟简称必须为 2 到 5 个字母、数字、点或短横线。"));
    }
    let taken: bool = db
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM game.alliances WHERE NOT deleted AND (lower(name) = lower($1) OR upper(short_name) = $2))",
            &[&name, &short_name],
        )
        .await
        .map_err(db_err)?
        .get(0);
    if taken {
        return Err(notify_error("该联盟名称或简称已被占用。"));
    }
    let _trading = super::market::lock().await;
    super::corpwallet::corp_debit(db, world, corp, ACCOUNT_CASH, CREATION_COST).await?;
    let id: i64 = db
        .query_one(
            "INSERT INTO game.alliances (name, short_name, description, url, executor_corp_id, creator_corp_id, creator_char_id, created)
             VALUES ($1, $2, $3, $4, $5, $5, $6, $7) RETURNING alliance_id",
            &[&name, &short_name, &description, &url, &corp, &me, &filetime_now()],
        )
        .await
        .map_err(db_err)?
        .get(0);
    db.execute("DELETE FROM game.alliance_applications WHERE corporation_id = $1", &[&corp]).await.map_err(db_err)?;
    set_alliance(db, world, corp, Some(id)).await?;
    info!("alliances: {me} of {corp} founded {id} <{short_name}> {name}");
    Ok(Value::Int(id))
}

/// Take `corp` out of its alliance: a new executor if it was one, or the alliance closes
/// with its last member.
pub(super) async fn leave_alliance(db: &Db, world: &World, corp: i64) -> Result<(), CallError> {
    let Some(r) = db
        .query_opt(
            "SELECT co.alliance_id, a.executor_corp_id FROM game.corporations co JOIN game.alliances a ON a.alliance_id = co.alliance_id
             WHERE co.corporation_id = $1",
            &[&corp],
        )
        .await
        .map_err(db_err)?
    else {
        return Ok(());
    };
    let (alliance, executor): (i64, Option<i64>) = (r.get(0), r.get(1));
    let old_member = member_lines(db, alliance).await?.into_iter().find(|(c, _)| *c == corp).map(|(_, l)| l);
    let old_alliance = alliance_line_of(db, alliance).await?;
    member_changed_before_leaving(db, world, alliance, corp, old_member).await?;
    set_alliance(db, world, corp, None).await?;
    db
        .execute("UPDATE game.corporations SET chosen_executor_id = NULL WHERE alliance_id = $1 AND chosen_executor_id = $2", &[&alliance, &corp])
        .await
        .map_err(db_err)?;
    if executor == Some(corp) {
        // The largest remaining member takes over; none left closes the alliance.
        let next: Option<i64> = db
            .query_opt(
                "SELECT co.corporation_id FROM game.corporations co WHERE co.alliance_id = $1 AND NOT co.deleted
                 ORDER BY (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = co.corporation_id AND NOT ch.deleted) DESC, co.corporation_id
                 LIMIT 1",
                &[&alliance],
            )
            .await
            .map_err(db_err)?
            .map(|r| r.get(0));
        match next {
            Some(next) => {
                db.execute("UPDATE game.alliances SET executor_corp_id = $2 WHERE alliance_id = $1", &[&alliance, &next]).await.map_err(db_err)?;
                info!("alliances: {next} is now executor of {alliance}");
            }
            None => {
                db.execute("UPDATE game.alliances SET deleted = true, executor_corp_id = NULL WHERE alliance_id = $1", &[&alliance]).await.map_err(db_err)?;
                db.execute("DELETE FROM game.alliance_applications WHERE alliance_id = $1", &[&alliance]).await.map_err(db_err)?;
                info!("alliances: {alliance} closed");
            }
        }
    }
    alliance_changed(db, world, alliance, old_alliance).await
}

/// OnAllianceMemberChanged for a corporation about to leave, sent while it can still hear it.
async fn member_changed_before_leaving(db: &Db, world: &World, alliance: i64, corp: i64, old: Option<Vec<Value>>) -> Result<(), CallError> {
    let ch = change(MEMBER_COLUMNS, old.as_deref(), None);
    tell(db, world, alliance, Some(corp), "OnAllianceMemberChanged", vec![Value::Int(alliance), Value::Int(corp), ch]).await
}

/// Join an accepted corporation to the alliance.
async fn admit(db: &Db, world: &World, alliance: i64, corp: i64) -> Result<(), CallError> {
    let allied: Option<i64> = db
        .query_opt("SELECT alliance_id FROM game.corporations WHERE corporation_id = $1 AND NOT deleted", &[&corp])
        .await
        .map_err(db_err)?
        .ok_or_else(|| notify_error("该军团已不存在。"))?
        .get(0);
    if allied.is_some() {
        return Err(notify_error("该军团已经在一个联盟中。"));
    }
    let old_alliance = alliance_line_of(db, alliance).await?;
    set_alliance(db, world, corp, Some(alliance)).await?;
    // Other applications of a corporation that joined lapse.
    let others = db
        .query(&format!("{APPLICATION_SELECT} WHERE corporation_id = $1 AND alliance_id <> $2"), &[&corp, &alliance])
        .await
        .map_err(db_err)?;
    db.execute("DELETE FROM game.alliance_applications WHERE corporation_id = $1 AND alliance_id <> $2", &[&corp, &alliance]).await.map_err(db_err)?;
    for r in &others {
        application_changed(db, world, r.get(0), corp, Some(application_line(r))).await?;
    }
    member_changed(db, world, alliance, corp, None).await?;
    alliance_changed(db, world, alliance, old_alliance).await
}

/// The alliance a bound allianceRegistry is for (bindParams `(allianceID, isMaster)`).
fn bound_alliance(c: &Client, call: &Call<'_>) -> Option<i64> {
    call.bind_params
        .and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int())))
        .or_else(|| c.session.int("allianceid"))
}

/// A director of the alliance's executor corporation.
async fn executor_director(db: &Db, me: i64, alliance: i64) -> Result<bool, CallError> {
    let (corp, roles) = membership(db, me).await?;
    let executor: Option<i64> = db
        .query_opt("SELECT executor_corp_id FROM game.alliances WHERE alliance_id = $1 AND NOT deleted", &[&alliance])
        .await
        .map_err(db_err)?
        .and_then(|r| r.get(0));
    Ok(executor == Some(corp) && roles & ROLE_DIRECTOR != 0)
}

/// A member corporation backs `candidate` as executor; one backed by corporations holding
/// more than half the alliance's pilots takes over.
async fn support_executor(db: &Db, world: &World, me: i64, alliance: i64, candidate: i64) -> Result<(), CallError> {
    let (corp, _) = membership(db, me).await?;
    if ceo_of(db, corp).await? != Some(me) {
        return Err(notify_error("只有军团 CEO 才能选择支持的执行军团。"));
    }
    let members = member_lines(db, alliance).await?;
    if !members.iter().any(|(c, _)| *c == candidate) || !members.iter().any(|(c, _)| *c == corp) {
        return Err(notify_error("该军团不是联盟成员。"));
    }
    let old = members.iter().find(|(c, _)| *c == corp).map(|(_, l)| l.clone());
    db.execute("UPDATE game.corporations SET chosen_executor_id = $2 WHERE corporation_id = $1", &[&corp, &candidate]).await.map_err(db_err)?;
    member_changed(db, world, alliance, corp, old).await?;
    let r = db
        .query_one(
            "SELECT COALESCE(sum(n) FILTER (WHERE co.chosen_executor_id = $2), 0)::bigint, COALESCE(sum(n), 0)::bigint
             FROM game.corporations co
             CROSS JOIN LATERAL (SELECT count(*) AS n FROM game.characters ch WHERE ch.corporation_id = co.corporation_id AND NOT ch.deleted) m
             WHERE co.alliance_id = $1 AND NOT co.deleted",
            &[&alliance, &candidate],
        )
        .await
        .map_err(db_err)?;
    let (backing, total): (i64, i64) = (r.get(0), r.get(1));
    if backing * MAJORITY_DENOMINATOR > total {
        let old_alliance = alliance_line_of(db, alliance).await?;
        let changed = db
            .execute("UPDATE game.alliances SET executor_corp_id = $2 WHERE alliance_id = $1 AND executor_corp_id IS DISTINCT FROM $2", &[&alliance, &candidate])
            .await
            .map_err(db_err)?;
        if changed > 0 {
            info!("alliances: {candidate} elected executor of {alliance}");
            alliance_changed(db, world, alliance, old_alliance).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- calls

/// Server suggestion algorithm shared by corporation tickers and alliance short names.
pub(super) fn suggested_names(name: &str) -> Vec<String> {
    let initials: String = name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_ascii_alphanumeric())).collect::<String>().to_uppercase();
    let compact: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase();
    let mut out: Vec<String> = vec![];
    for t in [initials.chars().take(SUGGESTION_MAX_LENGTH).collect::<String>(), compact.chars().take(SUGGESTION_MAX_LENGTH).collect(), compact.chars().take(SUGGESTION_FALLBACK_LENGTHS[0]).collect(), compact.chars().take(SUGGESTION_FALLBACK_LENGTHS[1]).collect()] {
        if t.len() >= SUGGESTION_MIN_LENGTH && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// GetSuggestedAllianceShortNames(name) -> [KeyVal(shortName)]
fn suggested_short_names(name: &str) -> Value {
    Value::list(suggested_names(name).into_iter().map(|t| keyval([("shortName", Value::unicode(t))])))
}

/// The alliance side of corpRegistry.
async fn corp_registry(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "CreateAlliance" => create(c, call).await,
        "GetSuggestedAllianceShortNames" => Ok(suggested_short_names(&trimmed_arg_text(call, 0))),
        // ApplyToJoinAlliance(allianceID, applicationText)
        "ApplyToJoinAlliance" => {
            let (alliance, text) = (int(0), trimmed_arg_text(call, 1));
            let corp = match unallied_ceo_corp(db, me).await {
                Ok(corp) => corp,
                Err(e) => return Some(Err(e)),
            };
            match db.query_opt("SELECT 1 FROM game.alliances WHERE alliance_id = $1 AND NOT deleted", &[&alliance]).await {
                Ok(Some(_)) => {}
                Ok(None) => return Some(Err(notify_error("该联盟已不存在。"))),
                Err(e) => return Some(Err(db_err(e))),
            }
            let old = match application_line_of(db, alliance, corp).await {
                Ok(old) => old,
                Err(e) => return Some(Err(e)),
            };
            if old.as_ref().is_some_and(|l| l[4].as_int() == Some(APP_NEW as i64)) {
                return Some(Err(CallError::User { msg: "CantApplyTwiceToAlliance".into(), dict: vec![] }));
            }
            let stored = db
                .execute(
                    "INSERT INTO game.alliance_applications (alliance_id, corporation_id, application_text, state, created) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (alliance_id, corporation_id) DO UPDATE SET application_text = $3, state = $4, created = $5",
                    &[&alliance, &corp, &text, &APP_NEW, &filetime_now()],
                )
                .await
                .map_err(db_err);
            match stored {
                Ok(_) => application_changed(db, world, alliance, corp, old).await.map(|_| Value::None),
                Err(e) => Err(e),
            }
        }
        // {allianceID: application} of the session's corporation.
        "GetAllianceApplications" => {
            let corp = c.session.int("corpid").unwrap_or(0);
            match db.query(&format!("{APPLICATION_SELECT} WHERE corporation_id = $1"), &[&corp]).await {
                Ok(rows) => {
                    let header = application_header();
                    let rows = rows.iter().map(|r| (r.get::<_, i64>(0), header.row(&application_line(r)))).collect();
                    Ok(indexed_rowset(&header, "allianceID", rows))
                }
                Err(e) => Err(db_err(e)),
            }
        }
        "DeleteAllianceApplication" => {
            let alliance = int(0);
            let corp = c.session.int("corpid").unwrap_or(0);
            if ceo_of(db, corp).await.ok().flatten() != Some(me) {
                return Some(Err(notify_error("只有军团 CEO 才能撤回联盟申请。")));
            }
            let old = match application_line_of(db, alliance, corp).await {
                Ok(old) => old,
                Err(e) => return Some(Err(e)),
            };
            match db.execute("DELETE FROM game.alliance_applications WHERE alliance_id = $1 AND corporation_id = $2", &[&alliance, &corp]).await {
                Ok(_) if old.is_some() => application_changed(db, world, alliance, corp, old).await.map(|_| Value::None),
                Ok(_) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        _ => return None,
    })
}

/// allianceRegistry, unbound (public) and bound to the session's alliance.
async fn alliance_registry(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let (db, world) = (&c.db, &*c.world);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    // ---- public ----
    match call.method {
        "GetAlliance" if !call.bound => return Some(alliance_row(db, int(0)).await),
        "GetAlliancePublicInfo" => return Some(alliance_row(db, int(0)).await),
        // GetRankedAlliances(maxLen): the largest alliances by pilots.
        "GetRankedAlliances" => {
            let limit = call.args.first().and_then(|v| v.as_int()).unwrap_or(DEFAULT_RANKED_LIMIT).clamp(1, MAX_RANKED_LIMIT);
            return Some(match db.query(&format!("{ALLIANCE_SELECT} WHERE NOT a.deleted ORDER BY 12 DESC, a.alliance_id LIMIT $1"), &[&limit]).await {
                Ok(rows) => {
                    let header = alliance_header();
                    Ok(crowset(&header, rows.iter().map(|r| header.row(&alliance_line(r))).collect()))
                }
                Err(e) => Err(db_err(e)),
            });
        }
        // [row(corporationID, ...)] of an alliance's members (info window).
        "GetAllianceMembers" => {
            return Some(member_lines(db, int(0)).await.map(|lines| {
                let header = member_header();
                crowset(&header, lines.into_iter().map(|(_, l)| header.row(&l)).collect())
            }))
        }
        // GetEmploymentRecord(corporationID): its alliances, newest first, then its founding
        // (the info window leaves the last entry out).
        "GetEmploymentRecord" => {
            let corp = int(0);
            if !is_player_corp(corp) {
                return Some(Ok(Value::list([])));
            }
            let rows = db
                .query(
                    "SELECT h.alliance_id, h.start_date, COALESCE(a.deleted, false) FROM game.alliance_history h
                     LEFT JOIN game.alliances a ON a.alliance_id = h.alliance_id WHERE h.corporation_id = $1
                     UNION ALL SELECT NULL, created, false FROM game.corporations WHERE corporation_id = $1
                     ORDER BY 2 DESC",
                    &[&corp],
                )
                .await;
            return Some(match rows {
                Ok(rows) => {
                    let header = DbRowDescriptor::new([("allianceID", DbType::I4), ("startDate", DbType::FileTime), ("deleted", DbType::Bool)]);
                    Ok(crowset(
                        &header,
                        rows.iter()
                            .map(|r| header.row(&[r.get::<_, Option<i64>>(0).map(Value::Int).unwrap_or(Value::None), Value::Int(r.get(1)), Value::Bool(r.get(2))]))
                            .collect(),
                    ))
                }
                Err(e) => Err(db_err(e)),
            });
        }
        _ => {}
    }
    // ---- bound to an alliance ----
    let alliance = bound_alliance(c, call)?;
    Some(match call.method {
        "GetAlliance" => alliance_row(db, alliance).await,
        // UpdateAlliance(description, url)
        "UpdateAlliance" => {
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error("只有执行军团的董事才能修改联盟信息。")));
            }
            let old = alliance_line_of(db, alliance).await.ok().flatten();
            match db
                .execute("UPDATE game.alliances SET description = $2, url = $3 WHERE alliance_id = $1", &[&alliance, &trimmed_arg_text(call, 0), &trimmed_arg_text(call, 1)])
                .await
            {
                Ok(_) => alliance_changed(db, world, alliance, old).await.map(|_| Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        "GetMembers" => member_lines(db, alliance).await.map(|lines| {
            let header = member_header();
            indexed_rowset(&header, "corporationID", lines.into_iter().map(|(corp, l)| (corp, header.row(&l))).collect())
        }),
        // {corporationID: application}
        "GetApplications" => match db.query(&format!("{APPLICATION_SELECT} WHERE alliance_id = $1"), &[&alliance]).await {
            Ok(rows) => {
                let header = application_header();
                let rows = rows.iter().map(|r| (r.get::<_, i64>(1), header.row(&application_line(r)))).collect();
                Ok(indexed_rowset(&header, "corporationID", rows))
            }
            Err(e) => Err(db_err(e)),
        },
        // UpdateApplication(corpID, applicationText, state): accepting admits the corporation.
        "UpdateApplication" | "GMForceAcceptApplication" => {
            let corp = int(0);
            let state = if call.method == "GMForceAcceptApplication" { APP_ACCEPTED } else { int(2) as i32 };
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error("只有执行军团的董事才能处理联盟申请。")));
            }
            let old = match application_line_of(db, alliance, corp).await {
                Ok(Some(old)) => old,
                Ok(None) => return Some(Err(notify_error("该申请已不存在。"))),
                Err(e) => return Some(Err(e)),
            };
            if !matches!(state, APP_ACCEPTED | APP_REJECTED) {
                return Some(Err(notify_error("无效的申请状态。")));
            }
            if state == APP_ACCEPTED {
                if let Err(e) = admit(db, world, alliance, corp).await {
                    return Some(Err(e));
                }
            }
            let stored = if state == APP_ACCEPTED { APP_EFFECTIVE } else { APP_REJECTED };
            match db
                .execute("UPDATE game.alliance_applications SET state = $3 WHERE alliance_id = $1 AND corporation_id = $2", &[&alliance, &corp, &stored])
                .await
            {
                Ok(_) => application_changed(db, world, alliance, corp, Some(old)).await.map(|_| Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        "DeclareExecutorSupport" => support_executor(db, world, me, alliance, int(0)).await.map(|_| Value::None),
        // DeleteMember(corpID): leave (the corporation's CEO) or remove (executor directors).
        "DeleteMember" => {
            let corp = int(0);
            let (my_corp, _) = match membership(db, me).await {
                Ok(m) => m,
                Err(e) => return Some(Err(e)),
            };
            let allowed = if corp == my_corp {
                ceo_of(db, corp).await.ok().flatten() == Some(me)
            } else {
                executor_director(db, me, alliance).await.unwrap_or(false)
            };
            if !allowed {
                return Some(Err(notify_error("你无法将该军团移出联盟。")));
            }
            match member_lines(db, alliance).await {
                Ok(lines) if lines.iter().any(|(c, _)| *c == corp) => leave_alliance(db, world, corp).await.map(|_| Value::None),
                Ok(_) => Err(notify_error("该军团不是联盟成员。")),
                Err(e) => Err(e),
            }
        }
        // {toID: row(toID, relationship)}
        "GetRelationships" => match db.query("SELECT to_id, relationship FROM game.alliance_relationships WHERE alliance_id = $1", &[&alliance]).await {
            Ok(rows) => {
                let header = relationship_header();
                let rows = rows.iter().map(|r| (r.get::<_, i64>(0), header.row(&[Value::Int(r.get(0)), Value::Int(r.get::<_, i32>(1) as i64)]))).collect();
                Ok(indexed_rowset(&header, "toID", rows))
            }
            Err(e) => Err(db_err(e)),
        },
        // SetRelationship(relationship, toID) / DeleteRelationship(toID)
        "SetRelationship" | "DeleteRelationship" => {
            if !executor_director(db, me, alliance).await.unwrap_or(false) {
                return Some(Err(notify_error("只有执行军团的董事才能设置联盟关系。")));
            }
            let (relationship, to) = if call.method == "SetRelationship" { (Some(int(0) as i32), int(1)) } else { (None, int(0)) };
            let old: Option<i32> = match db.query_opt("SELECT relationship FROM game.alliance_relationships WHERE alliance_id = $1 AND to_id = $2", &[&alliance, &to]).await {
                Ok(r) => r.map(|r| r.get(0)),
                Err(e) => return Some(Err(db_err(e))),
            };
            let stored = match relationship {
                Some(rel) => {
                    db
                        .execute(
                            "INSERT INTO game.alliance_relationships (alliance_id, to_id, relationship) VALUES ($1, $2, $3)
                             ON CONFLICT (alliance_id, to_id) DO UPDATE SET relationship = $3",
                            &[&alliance, &to, &rel],
                        )
                        .await
                }
                None => db.execute("DELETE FROM game.alliance_relationships WHERE alliance_id = $1 AND to_id = $2", &[&alliance, &to]).await,
            };
            if let Err(e) = stored {
                return Some(Err(db_err(e)));
            }
            let line = |rel: Option<i32>| rel.map(|r| vec![Value::Int(to), Value::Int(r as i64)]);
            let ch = change(RELATIONSHIP_COLUMNS, line(old).as_deref(), line(relationship).as_deref());
            tell(db, world, alliance, None, "OnAllianceRelationshipChanged", vec![Value::Int(alliance), Value::Int(to), ch]).await.map(|_| Value::None)
        }
        "DeclareWarAgainst" => Err(notify_error("战争尚未开放。")),
        // The alliance's bills (services/bills), paid by the executor's accountants.
        "GetBills" => super::bills::bills(db, alliance, false).await,
        "GetBillsReceivable" => super::bills::bills(db, alliance, true).await,
        "GetBillBalance" => match db.query_opt("SELECT amount::float8 FROM game.bills WHERE bill_id = $1 AND debtor_id = $2", &[&int(0), &alliance]).await {
            Ok(r) => Ok(Value::Float(r.map(|r| r.get(0)).unwrap_or(0.0))),
            Err(e) => Err(db_err(e)),
        },
        // PayBill(billID, fromAccountKey): from the executor corporation's wallet.
        "PayBill" => {
            let (bill, key) = (int(0), call.kwargs.get("fromAccountKey").and_then(|v| v.as_int()).unwrap_or(int(1)) as i32);
            let corp = c.session.int("corpid").unwrap_or(0);
            let owed = match db.query_opt("SELECT 1 FROM game.bills WHERE bill_id = $1 AND debtor_id = $2", &[&bill, &alliance]).await {
                Ok(r) => r.is_some(),
                Err(e) => return Some(Err(db_err(e))),
            };
            let allowed = executor_director(db, me, alliance).await.unwrap_or(false)
                || (super::bills::is_accountant(db, me, corp).await.unwrap_or(false)
                    && db.query_opt("SELECT 1 FROM game.alliances WHERE alliance_id = $1 AND executor_corp_id = $2", &[&alliance, &corp]).await.ok().flatten().is_some());
            if !owed || !allowed || !super::corpwallet::may_take(db, me, corp, key).await.unwrap_or(false) {
                return Some(Err(notify_error("你无法支付该账单。")));
            }
            super::bills::pay(db, world, bill, Some(corp), key).await.map(|_| Value::None)
        }
        _ => return None,
    })
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.service {
        "corpRegistry" => corp_registry(c, call).await,
        "allianceRegistry" => alliance_registry(c, call).await,
        _ => None,
    }
}
