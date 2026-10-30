//! scanMgr service; see the parent feature for data sources and limitations.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(Ok(match (call.service, call.method) {
        ("scanMgr", "GetSystemScanMgr") => c.bind("scanMgr", Value::None),

        _ => return None,
    }))
}
