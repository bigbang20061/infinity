//! Periodic pilot position persistence for destiny's ballpark (michelle.py).
//! Uses game.items with guarded writes on the system worker, including shutdown flushes;
//! the snapshot interval is operator-configured. Other ball state and persistent
//! timers are not supported.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub(super) struct Context {
    pub tx: mpsc::WeakUnboundedSender<actor::Job>,
    pub gate: Arc<tokio::sync::Mutex<()>>,
    pub written: Arc<AtomicU64>,
    pub db: Db,
}

#[derive(Clone, Copy)]
struct Position { id: i64, generation: u64, pos: V3 }

fn candidates(s: &System) -> impl Iterator<Item = (i64, u64)> + '_ {
    s.pilots().filter_map(|ball| {
        let &(generation, previous) = s.snapshot_positions.get(&ball.id)?;
        (ball.pos != previous).then_some((ball.id, generation))
    })
}

pub(super) fn schedule(s: &System) {
    if candidates(s).next().is_none() { return; }
    let Some(ctx) = &s.snapshot_context else { return };
    let Ok(guard) = ctx.gate.clone().try_lock_owned() else { return };
    let Some(tx) = ctx.tx.upgrade() else { return };
    let handle = SystemHandle { tx, gate: ctx.gate.clone(), written: ctx.written.clone() };
    let db = ctx.db.clone();
    s.defer(async move {
        let _guard = guard;
        if let Err(error) = write_positions(&handle, &db).await {
            tracing::warn!(%error, "position snapshot failed");
        }
    });
}

pub(super) async fn flush(handle: SystemHandle, db: Db) -> Result<u64> {
    let before = handle.written.load(Ordering::SeqCst);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let worker_handle = handle.clone();
    handle.send(Box::new(move |s| s.defer(async move {
        let _guard = worker_handle.gate.lock().await;
        let result = write_positions(&worker_handle, &db).await
            .map(|_| worker_handle.written.load(Ordering::SeqCst) - before);
        let _ = tx.send(result);
    }))).map_err(|_| anyhow::anyhow!("system actor stopped before position flush"))?;
    rx.await.map_err(|_| anyhow::anyhow!("position flush worker unavailable"))?
}

async fn write_positions(handle: &SystemHandle, db: &Db) -> Result<u64> {
    let (system, candidates) = handle.with(|s| (s.id, candidates(s).collect::<Vec<_>>())).await
        .ok_or_else(|| anyhow::anyhow!("system actor stopped during position snapshot"))?;
    if candidates.is_empty() { return Ok(0); }
    let ids: Vec<i64> = candidates.iter().map(|&(id, _)| id).collect();
    let tokens = crate::dao::items::positions::versions(db, &ids, system).await?;
    if tokens.is_empty() { return Ok(0); }
    let candidates: HashMap<i64, u64> = candidates.into_iter().collect();
    let positions = handle.with(move |s| {
        s.pilots().filter_map(|ball| {
            let &(generation, previous) = s.snapshot_positions.get(&ball.id)?;
            (ball.pos != previous && candidates.get(&ball.id) == Some(&generation))
                .then(|| Position { id: ball.id, generation, pos: ball.pos })
        }).collect::<Vec<_>>()
    }).await.ok_or_else(|| anyhow::anyhow!("system actor stopped during position snapshot"))?;
    let positions: Vec<_> = positions.into_iter().filter(|p| tokens.contains_key(&p.id)).collect();
    if positions.is_empty() { return Ok(0); }
    let ids: Vec<i64> = positions.iter().map(|p| p.id).collect();
    let versions: Vec<&str> = positions.iter().map(|p| tokens[&p.id].as_str()).collect();
    let xs: Vec<f64> = positions.iter().map(|p| p.pos[0]).collect();
    let ys: Vec<f64> = positions.iter().map(|p| p.pos[1]).collect();
    let zs: Vec<f64> = positions.iter().map(|p| p.pos[2]).collect();
    // xmin changes on every row write, so later writers win without coordination or schema changes.
    let succeeded = crate::dao::items::positions::save_snapshot(db, &ids, &versions, &xs, &ys, &zs, system).await?;
    let count = succeeded.len() as u64;
    handle.with(move |s| acknowledge(s, positions, &succeeded)).await
        .ok_or_else(|| anyhow::anyhow!("system actor stopped acknowledging position snapshot"))?;
    handle.written.fetch_add(count, Ordering::SeqCst);
    Ok(count)
}

fn acknowledge(s: &mut System, positions: Vec<Position>, succeeded: &std::collections::HashSet<i64>) {
    for p in positions {
        if succeeded.contains(&p.id) {
            if let Some((generation, previous)) = s.snapshot_positions.get_mut(&p.id) {
                if *generation == p.generation { *previous = p.pos; }
            }
        }
    }
}

#[cfg(test)] mod tests;
