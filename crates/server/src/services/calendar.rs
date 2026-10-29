//! The calendar (calendarProxy, calendarMgr; eve/client/script/ui/services/eveCalendarsvc.py).
//!
//! Events (game.calendar_events) belong to a character (personal, with invitees) or to a
//! player corporation (seen by its members; created by its directors). Each invitee has a
//! response (game.calendar_responses, appConst.eventResponse*); the owner of a personal event
//! is attending. Online invitees hear of new, changed and cancelled events. Alliance events
//! aren't supported.

use super::{db_err, keyval, notify_error, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::ROLE_DIRECTOR;
use marshal::{DbRowDescriptor, DbType, Value};

// appConst.eventResponse*
const RESPONSE_DELETED: i32 = 1;
const RESPONSE_UNDECIDED: i32 = 3;
const RESPONSE_ACCEPTED: i32 = 4;
const MAX_INVITEES: usize = 50;

fn text(v: Option<&Value>, max: usize) -> String {
    v.and_then(|v| v.as_text()).map(|t| t.chars().take(max).collect()).unwrap_or_default()
}

/// Events the character sees in a month: their own, their corporation's, and those they're invited to.
async fn event_list(c: &Client, month: i64, year: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let corp = c.session.int("corpid").unwrap_or(0);
    let rows = c
        .db
        .client
        .query(
            "SELECT e.event_id, e.owner_id, e.event_date, e.modified, e.duration::bigint, e.importance::bigint, e.title, e.deleted
             FROM game.calendar_events e
             WHERE (e.owner_id IN ($1, $2) OR EXISTS (SELECT 1 FROM game.calendar_responses r WHERE r.event_id = e.event_id AND r.char_id = $1 AND r.status <> $5))
               AND extract(month FROM to_timestamp((e.event_date - 116444736000000000) / 10000000.0))::int = $3
               AND extract(year FROM to_timestamp((e.event_date - 116444736000000000) / 10000000.0))::int = $4
             ORDER BY e.event_date",
            &[&me, &corp, &(month as i32), &(year as i32), &RESPONSE_DELETED],
        )
        .await
        .map_err(db_err)?;
    let header = DbRowDescriptor::new([
        ("eventID", DbType::I4),
        ("ownerID", DbType::I4),
        ("eventDateTime", DbType::FileTime),
        ("dateModified", DbType::FileTime),
        ("eventDuration", DbType::I4),
        ("importance", DbType::I4),
        ("eventTitle", DbType::WStr),
        ("isDeleted", DbType::Bool),
    ]);
    let lines: Vec<Value> = rows
        .iter()
        .map(|r| {
            header.row(&[
                Value::Int(r.get(0)),
                Value::Int(r.get(1)),
                Value::Int(r.get(2)),
                Value::Int(r.get(3)),
                Value::Int(r.get(4)),
                Value::Int(r.get(5)),
                Value::unicode(r.get::<_, String>(6)),
                Value::Bool(r.get(7)),
            ])
        })
        .collect();
    Ok(Value::list([Value::List(lines)]))
}

/// Invitees of an event, but its owner.
async fn invitees(c: &Client, event: i64) -> Result<Vec<i64>, CallError> {
    let rows = c.db.client.query("SELECT char_id FROM game.calendar_responses WHERE event_id = $1 AND status <> $2", &[&event, &RESPONSE_DELETED]).await.map_err(db_err)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

/// Create an event owned by `owner` (the character, or their corporation).
async fn create(c: &Client, call: &Call<'_>, owner: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let when = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let duration = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let (title, description) = (text(call.args.get(2), 100), text(call.args.get(3), 10000));
    let important = call.args.get(4).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let now = filetime_now();
    let id: i64 = c
        .db
        .client
        .query_one(
            "INSERT INTO game.calendar_events (owner_id, creator_id, event_date, duration, title, description, importance, modified)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING event_id",
            &[&owner, &me, &when, &duration, &title, &description, &important, &now],
        )
        .await
        .map_err(db_err)?
        .get(0);
    if owner == me {
        let guests: Vec<i64> = call.args.get(5).and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).filter(|g| *g != me).take(MAX_INVITEES).collect();
        c.db.client.execute("INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3)", &[&id, &me, &RESPONSE_ACCEPTED]).await.map_err(db_err)?;
        for g in &guests {
            c.db.client.execute("INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", &[&id, g, &RESPONSE_UNDECIDED]).await.map_err(db_err)?;
            c.world.notify_pilot(*g, "OnNewCalendarEvent", vec![Value::Int(id), Value::Int(owner), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)]);
        }
    }
    Ok(Value::Int(id))
}

/// The character may change `owner`'s events: their own, or their corporation's as a director.
fn may_manage(c: &Client, owner: i64) -> bool {
    let me = c.session.int("charid").unwrap_or(0);
    owner == me || (c.session.int("corpid") == Some(owner) && c.session.int("corprole").unwrap_or(0) & ROLE_DIRECTOR != 0)
}

/// `Edit*Event(eventID, oldDateTime, dateTime, duration, title, description, important)`.
async fn edit(c: &Client, call: &Call<'_>) -> CallResult {
    let event = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let Some(owner) = c.db.client.query_opt("SELECT owner_id FROM game.calendar_events WHERE event_id = $1", &[&event]).await.map_err(db_err)?.map(|r| r.get::<_, i64>(0)) else {
        return Err(notify_error("该活动已不存在。"));
    };
    if !may_manage(c, owner) {
        return Err(notify_error("你无法修改该活动。"));
    }
    let old = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    let when = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(old);
    let duration = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let title = text(call.args.get(4), 100);
    let important = call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    c.db.client
        .execute(
            "UPDATE game.calendar_events SET event_date = $2, duration = $3, title = $4, description = $5, importance = $6, modified = $7 WHERE event_id = $1",
            &[&event, &when, &duration, &title, &text(call.args.get(5), 10000), &important, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    for g in invitees(c, event).await? {
        c.world.notify_pilot(
            g,
            "OnEditCalendarEvent",
            vec![Value::Int(event), Value::Int(owner), Value::Int(old), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)],
        );
    }
    Ok(Value::None)
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match (call.service, call.method) {
        // GetEventList(month, year) -> [[row]]
        ("calendarProxy", "GetEventList") => event_list(c, int(0), int(1)).await,
        // GetEventDetails(eventID, ownerID)
        ("calendarProxy", "GetEventDetails") => {
            match c.db.client.query_opt("SELECT description, creator_id, owner_id FROM game.calendar_events WHERE event_id = $1", &[&int(0)]).await {
                Ok(Some(r)) => Ok(keyval([
                    ("eventID", Value::Int(int(0))),
                    ("eventText", Value::unicode(r.get::<_, String>(0))),
                    ("creatorID", Value::Int(r.get(1))),
                    ("ownerID", Value::Int(r.get(2))),
                ])),
                Ok(None) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }
        // CreatePersonalEvent(dateTime, duration, title, description, important, invitees) -> eventID
        ("calendarMgr", "CreatePersonalEvent") => create(c, call, me).await,
        ("calendarMgr", "CreateCorporationEvent") => {
            let corp = c.session.int("corpid").unwrap_or(0);
            if !super::corps::is_player_corp(corp) || !may_manage(c, corp) {
                return Some(Err(notify_error("只有玩家军团的董事才能创建军团活动。")));
            }
            create(c, call, corp).await
        }
        ("calendarMgr", "CreateAllianceEvent") | ("calendarMgr", "EditAllianceEvent") => Err(notify_error("暂不支持联盟活动。")),
        ("calendarMgr", "EditPersonalEvent") | ("calendarMgr", "EditCorporationEvent") => edit(c, call).await,
        // DeleteEvent(eventID, ownerID): the owner cancels it; an invitee drops it.
        ("calendarMgr", "DeleteEvent") => {
            let event = int(0);
            let found = match c.db.client.query_opt("SELECT owner_id, event_date FROM game.calendar_events WHERE event_id = $1", &[&event]).await {
                Ok(r) => r.map(|r| (r.get::<_, i64>(0), r.get::<_, i64>(1))),
                Err(e) => return Some(Err(db_err(e))),
            };
            let Some((owner, when)) = found else { return Some(Ok(Value::None)) };
            if may_manage(c, owner) {
                let guests = match invitees(c, event).await {
                    Ok(g) => g,
                    Err(e) => return Some(Err(e)),
                };
                if let Err(e) = c.db.client.execute("UPDATE game.calendar_events SET deleted = true, modified = $2 WHERE event_id = $1", &[&event, &filetime_now()]).await {
                    return Some(Err(db_err(e)));
                }
                for g in guests {
                    c.world.notify_pilot(g, "OnRemoveCalendarEvent", vec![Value::Int(event), Value::Int(when), Value::Bool(true)]);
                }
                Ok(Value::None)
            } else {
                c.db.client
                    .execute("UPDATE game.calendar_responses SET status = $3 WHERE event_id = $1 AND char_id = $2", &[&event, &me, &RESPONSE_DELETED])
                    .await
                    .map(|_| Value::None)
                    .map_err(db_err)
            }
        }
        // SendEventResponse(eventID, ownerID, response)
        ("calendarMgr", "SendEventResponse") => c
            .db
            .client
            .execute(
                "INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3) ON CONFLICT (event_id, char_id) DO UPDATE SET status = $3",
                &[&int(0), &me, &(int(2) as i32)],
            )
            .await
            .map(|_| Value::None)
            .map_err(db_err),
        // [row(eventID, status)]
        ("calendarMgr", "GetResponsesForCharacter") => match c.db.client.query("SELECT event_id, status::bigint FROM game.calendar_responses WHERE char_id = $1", &[&me]).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("eventID", Value::Int(r.get(0))), ("status", Value::Int(r.get(1)))])))),
            Err(e) => Err(db_err(e)),
        },
        // GetResponsesToEvent(eventID, ownerID) -> [row(characterID, status)]
        ("calendarMgr", "GetResponsesToEvent") => match c.db.client.query("SELECT char_id, status::bigint FROM game.calendar_responses WHERE event_id = $1", &[&int(0)]).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("characterID", Value::Int(r.get(0))), ("status", Value::Int(r.get(1)))])))),
            Err(e) => Err(db_err(e)),
        },
        // UpdateEventParticipants(eventID, charsToAdd, charsToRemove)
        ("calendarMgr", "UpdateEventParticipants") => {
            let event = int(0);
            let owner = match c.db.client.query_opt("SELECT owner_id FROM game.calendar_events WHERE event_id = $1", &[&event]).await {
                Ok(r) => r.map(|r| r.get::<_, i64>(0)),
                Err(e) => return Some(Err(db_err(e))),
            };
            if owner != Some(me) {
                return Some(Err(notify_error("你无法修改该活动。")));
            }
            let list = |i: usize| call.args.get(i).and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).collect::<Vec<i64>>();
            for g in list(1) {
                if let Err(e) = c.db.client.execute("INSERT INTO game.calendar_responses (event_id, char_id, status) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", &[&event, &g, &RESPONSE_UNDECIDED]).await {
                    return Some(Err(db_err(e)));
                }
            }
            let gone = list(2);
            c.db.client
                .execute("DELETE FROM game.calendar_responses WHERE event_id = $1 AND char_id = ANY($2) AND char_id <> $3", &[&event, &gone, &me])
                .await
                .map(|_| Value::None)
                .map_err(db_err)
        }
        _ => return None,
    })
}
