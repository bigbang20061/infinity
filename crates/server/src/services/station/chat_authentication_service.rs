//! Chat authentication service; tokens are generated for the current session.
use super::*;
pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetAuthenticationToken" => {
            let char_id = c.session.int("charid").unwrap_or(0);
            Some(Ok(Value::str(c.chat.token_for(char_id))))
        }
        _ => None,
    }
}
