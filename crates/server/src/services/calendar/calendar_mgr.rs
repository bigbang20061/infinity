//! Calendar manager for `eve/client/script/ui/services/eveCalendarsvc.py`.
//! Uses `game.calendar_events` and `game.calendar_responses`; alliance events are unsupported.
use super::*;

async fn invitees(c: &Client, event: i64) -> Result<Vec<i64>, CallError> {
    dao::invitees(&c.db, event, RESPONSE_DELETED).await.map_err(db_err)
}
async fn create(c: &Client, call: &Call<'_>, owner: i64) -> CallResult {
    let me = c.session.int("charid").unwrap_or(0);
    let when = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let duration = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let (title, description) = (value_text(call.args.get(2), MAX_TITLE_LENGTH), value_text(call.args.get(3), MAX_DESCRIPTION_LENGTH));
    let important = call.args.get(4).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let now = filetime_now();
    let guests: Vec<i64> = call.args.get(5).and_then(|v| v.items()).unwrap_or(&[]).iter().filter_map(|v| v.as_int()).filter(|g| *g != me).take(MAX_INVITEES).collect();
    let world = c.world.clone();
    c.db.transaction(
        |tx, outbox: &mut Outbox| {
            let title = title.clone();
            let description = description.clone();
            let guests = guests.clone();
            Box::pin(async move {
                let id = dao::create(tx, &dao::NewEvent { owner, creator: me, when, duration, title: &title, description: &description, importance: important, modified: now }).await.map_err(db_err)?;
                if owner == me {
                    dao::initial_responses(tx, id, me, &guests, RESPONSE_UNDECIDED, RESPONSE_ACCEPTED).await.map_err(db_err)?;
                    for g in guests {
                        outbox.notify_pilot(g, "OnNewCalendarEvent", vec![Value::Int(id), Value::Int(owner), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)]);
                    }
                }
                Ok(Value::Int(id))
            })
        },
        |value, outbox| async move { dispatch_world(value, outbox, &world) },
    ).await
}
fn may_manage(c: &Client, owner: i64) -> bool {
    let me = c.session.int("charid").unwrap_or(0);
    owner == me || (c.session.int("corpid") == Some(owner) && c.session.int("corprole").unwrap_or(0) & ROLE_DIRECTOR != 0)
}
async fn edit(c: &Client, call: &Call<'_>) -> CallResult {
    let event = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let Some(owner) = dao::owner(&c.db, event).await.map_err(db_err)? else { return Err(notify_error(messages::CALENDAR_EVENT_NOT_FOUND)); };
    if !may_manage(c, owner) { return Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED)); }
    let old = call.args.get(1).and_then(|v| v.as_int()).unwrap_or(0);
    let when = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(old);
    let duration = call.args.get(3).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    let title = value_text(call.args.get(4), MAX_TITLE_LENGTH);
    let important = call.args.get(6).and_then(|v| v.as_int()).unwrap_or(0) as i32;
    dao::edit(&c.db, &dao::EditEvent { event, when, duration, title: &title, description: &value_text(call.args.get(5), MAX_DESCRIPTION_LENGTH), importance: important, modified: filetime_now() }).await.map_err(db_err)?;
    for g in invitees(c, event).await? {
        c.world.notify_pilot(g, "OnEditCalendarEvent", vec![Value::Int(event), Value::Int(owner), Value::Int(old), Value::Int(when), Value::Int(duration as i64), Value::unicode(title.as_str()), Value::Int(important as i64)]);
    }
    Ok(Value::None)
}
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match call.method {
        "CreatePersonalEvent" => create(c, call, me).await,
        "CreateCorporationEvent" => {
            let corp = c.session.int("corpid").unwrap_or(0);
            if !super::super::corps::is_player_corp(corp) || !may_manage(c, corp) { return Some(Err(notify_error(messages::PLAYER_CORP_DIRECTOR_REQUIRED_FOR_CORP_EVENT))); }
            create(c, call, corp).await
        }
        "EditPersonalEvent" | "EditCorporationEvent" => edit(c, call).await,
        "DeleteEvent" => {
            let event = int(0);
            let found = match dao::owner_date(&c.db, event).await { Ok(r) => r, Err(e) => return Some(Err(db_err(e))) };
            let Some(found) = found else { return Some(Ok(Value::None)) };
            if may_manage(c, found.owner_id) {
                let guests = match invitees(c, event).await { Ok(g) => g, Err(e) => return Some(Err(e)) };
                if let Err(e) = dao::delete_event(&c.db, event, filetime_now()).await { return Some(Err(db_err(e))); }
                for g in guests { c.world.notify_pilot(g, "OnRemoveCalendarEvent", vec![Value::Int(event), Value::Int(found.event_date), Value::Bool(true)]); }
                Ok(Value::None)
            } else { dao::delete_response(&c.db, event, me, RESPONSE_DELETED).await.map(|_| Value::None).map_err(db_err) }
        }
        "SendEventResponse" => dao::set_response(&c.db, int(0), me, int(2) as i32).await.map(|_| Value::None).map_err(db_err),
        "GetResponsesForCharacter" => match dao::character_responses(&c.db, me).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("eventID", Value::Int(r.event_id)), ("status", Value::Int(r.status))])))), Err(e) => Err(db_err(e)),
        },
        "GetResponsesToEvent" => match dao::event_responses(&c.db, int(0)).await {
            Ok(rows) => Ok(Value::list(rows.iter().map(|r| keyval([("characterID", Value::Int(r.char_id)), ("status", Value::Int(r.status))])))), Err(e) => Err(db_err(e)),
        },
        "UpdateEventParticipants" => {
            let event = int(0);
            let owner = match dao::owner(&c.db, event).await { Ok(r) => r, Err(e) => return Some(Err(db_err(e))) };
            if owner != Some(me) { return Some(Err(notify_error(messages::CALENDAR_EVENT_EDIT_NOT_AUTHORIZED))); }
            for g in value_id_list(call.args.get(1)) {
                if let Err(e) = dao::add_response(&c.db, event, g, RESPONSE_UNDECIDED).await { return Some(Err(db_err(e))); }
            }
            let gone = value_id_list(call.args.get(2));
            dao::remove_responses(&c.db, event, &gone, me).await.map(|_| Value::None).map_err(db_err)
        }
        _ => return None,
    })
}
