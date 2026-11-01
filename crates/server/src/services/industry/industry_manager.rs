//! `industryManager` dispatch for the client industry package, using industry jobs and blueprints.
//! Cancellation only succeeds while a job is installed.

use super::*;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    Some(match (call.service, call.method) {
        ("industryManager", "InstallJob") => match call.args.first() {
            Some(request) => install(c, request).await,
            None => Err(notify_error(messages::NO_PROJECT_TO_INSTALL)),
        },
        // GetJobsByOwner(ownerID, includeCompleted)
        ("industryManager", "GetJobsByOwner") => {
            let owner = int(0);
            let completed = call.args.get(1).is_some_and(|v| matches!(v, Value::Bool(true)) || v.as_int().is_some_and(|i| i != 0));
            let cutoff = if completed { filetime_now().saturating_sub(crate::config::game().industry.history_days.saturating_mul(FILETIME_DAY)) } else { 0 };
            match authorization::owner(&c.db, me, owner, None).await {
                Ok(()) => industry_dao::jobs_by_owner(&c.db, owner, completed, INDUSTRY_JOB_STATUS_INSTALLED, cutoff).await.map(|rows| Value::list(rows.iter().map(Job::keyval))).map_err(db_err),
                Err(e) => Err(e),
            }
        }
        ("industryManager", "GetJob") => match job(&c.db, int(0)).await {
            Ok(j) => authorization::owner(&c.db, me, j.owner, None).await.map(|()| j.keyval()),
            Err(e) => Err(e),
        },
        ("industryManager", "GetJobCounts") => {
            if int(0) != me { Err(user_error(messages::MSG_INDUSTRY_JOB_ACCESS_DENIED, vec![])) }
            else { job_counts(&c.db, me).await.map(|counts| Value::Dict(counts.into_iter().map(|(a, n)| (Value::Int(a as i64), Value::Int(n))).collect())) }
        }
        // CompleteJob(jobID, solarSystemID)
        ("industryManager", "CompleteJob") => complete(c, int(0)).await.map(|_| Value::None),
        // CompleteManyJobs([(jobID, solarSystemID)])
        ("industryManager", "CompleteManyJobs") => {
            let ids: Vec<i64> = call
                .args
                .first()
                .and_then(|v| v.items())
                .unwrap_or(&[])
                .iter()
                .filter_map(|p| p.items().and_then(|p| p.first()).and_then(|v| v.as_int()))
                .collect();
            for id in ids {
                match complete(c, id).await {
                    Ok(()) | Err(CallError::User { .. }) => {}
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::None)
        }
        // CancelJob(jobID, solarSystemID): the materials and cost are lost.
        ("industryManager", "CancelJob") => {
            let _trading = lock().await;
            let db = c.db.clone();
            let id = int(0);
            db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    let mut j = job(tx, id).await?;
                    authorization::owner(tx, me, j.owner, None).await?;
                    j.status = STATUS_CANCELLED;
                    j.completed = Some(filetime_now());
                    j.completed_by = Some(me);
                    if industry_dao::cancel_job(tx, &j, me, INDUSTRY_JOB_STATUS_INSTALLED).await.map_err(db_err)? == 0 {
                        return Err(notify_error(messages::INDUSTRY_PROJECT_CANNOT_CANCEL));
                    }
                    completion::cancel(tx, id).await?;
                    tell_job(tx, outbox, &j).await?;
                    Ok(Value::None)
                }),
                |value, outbox| async move { dispatch_client(value, outbox, c) },
            ).await
        }
        _ => return None,
    })
}
