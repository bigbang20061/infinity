//! Guest list for the client's `station` service.
//! Uses the current session, with no database query.
//! Other docked characters are not included.
use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = char_id(c);
    Some(match (call.service, call.method) {
        ("station", "GetGuests") => {
            let corp = c.session.get("corpid").clone();
            Ok(Value::list([Value::tuple([Value::Int(me), corp, Value::None, Value::None])]))
        }
        // No-op / empty answers for station systems we don't model yet.
        _ => return None,
    })
}
