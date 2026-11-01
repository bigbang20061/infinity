//! Individual kill reports and bound war statistics for `warStatisticMgr`.
//! Mirrors `warReport.py` and `killReportUtil.py`, using game killmails and wars.
//! Historical alliance snapshots may be absent; unsupported war features have no report.

use super::*;

pub(super) async fn dispatch(c: &Client, call: &Call<'_>) -> Option<CallResult> {
    match call.method {
        "GetKillMail" => {
            let id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
            Some(killmails(c, None, id, 1, None).await.map(|v| {
                v.items().and_then(|k| k.first().cloned()).unwrap_or(Value::None)
            }))
        }
        "GetBaseInfo" | "GetKills" | "GetKillsByGroup" =>
            Some(crate::services::wars::statistics::query(c, call).await),
        _ => None,
    }
}
