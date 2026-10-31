//! `industryManager` dispatch for the client industry package, using industry jobs and blueprints.
//! Corporation industry is not supported; cancellation only succeeds while a job is installed.

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
            match industry_dao::jobs_by_owner(&c.db, owner, completed, INDUSTRY_JOB_STATUS_INSTALLED, cutoff).await {
                Ok(rows) => Ok(Value::list(rows.iter().map(Job::keyval))),
                Err(e) => Err(db_err(e)),
            }
        }
        ("industryManager", "GetJob") => job(&c.db, int(0)).await.map(|j| j.keyval()),
        ("industryManager", "GetJobCounts") => {
            job_counts(&c.db, int(0)).await.map(|counts| Value::Dict(counts.into_iter().map(|(a, n)| (Value::Int(a as i64), Value::Int(n))).collect()))
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
            match job(&c.db, int(0)).await {
                Ok(mut j) if (j.owner == me || j.installer == me) && j.status == INDUSTRY_JOB_STATUS_INSTALLED => {
                    j.status = STATUS_CANCELLED;
                    j.completed = Some(filetime_now());
                    j.completed_by = Some(me);
                    match industry_dao::cancel_job(&c.db, &j, me, INDUSTRY_JOB_STATUS_INSTALLED).await {
                        Ok(0) => Err(notify_error(messages::INDUSTRY_PROJECT_CANNOT_CANCEL)),
                        Ok(_) => {
                            info!("industry: {me} cancelled job {}", j.id);
                            let mut outbox = Outbox::default();
                            tell_job(&mut outbox, &j);
                            dispatch_world(Value::None, outbox, &c.world)
                        }
                        Err(e) => Err(db_err(e)),
                    }
                }
                Ok(_) => Err(notify_error(messages::INDUSTRY_PROJECT_CANNOT_CANCEL)),
                Err(e) => Err(e),
            }
        }
        _ => return None,
    })
}
