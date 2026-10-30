//! corpRegistry service; see the parent feature for data sources and limitations.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let my_corp = c.session.int("corpid").unwrap_or(0);
    match (call.service, call.method) {
        ("corpRegistry", "GetCorporation") => {
            let corp = call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(my_corp);
            Some(corporation(c, corp).await)
        }
        _ => None,
    }
}
