use super::*;
use crate::consts::FLAG_HI_SLOT_FIRST;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

struct MiningFixture {
    f: Fixture,
    module: i64,
    module_type: i32,
    ore: i32,
    volume: f64,
    rock: i64,
}

impl MiningFixture {
    async fn new(quantity: i64, fraction: f64) -> Self {
        let f = Fixture::new().await;
        let ore = f.sde.types.iter().find(|t| t.category_id == Some(CATEGORY_ASTEROID) && t.volume.is_some_and(|v| v >= 0.01)).unwrap();
        let (ore, volume) = (ore.type_id, ore.volume.unwrap());
        let module_type = f.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_MODULE)).unwrap().type_id;
        let module = f.db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type, &f.owner, &f.ship, &FLAG_HI_SLOT_FIRST]).await.unwrap().get("item_id");
        for (item, attr, value) in [(module, ATTR_MINING_AMOUNT, volume * fraction), (module, ATTR_MAX_RANGE, 10000.0),
            (module, ATTR_CRIT_CHANCE, 0.0), (module, ATTR_WASTE_PROBABILITY, 0.0),
            (f.ship, ATTR_CAPACITY, volume * 10.0), (f.ship, ATTR_MINING_HOLD, 0.0)] {
            f.set(item, attr, value).await;
        }
        let rock = crate::dao::asteroids::insert(&f.db, f.system, f.system, ore, quantity, 1.0, [0.0; 3]).await.unwrap();
        state().rocks.insert(rock, Rock { system: f.system, field: f.system, type_id: ore, quantity });
        f.world.add_ship(f.system, rock_ball(rock, ore, [0.0; 3], 1.0)).await.unwrap();
        Self { f, module, module_type, ore, volume, rock }
    }

    async fn fit(&self) -> anyhow::Result<Fit> {
        Fit::load_in_space(&self.f.db, &self.f.world, self.f.system, self.f.owner, self.f.ship).await.map_err(|e| anyhow::anyhow!("fit: {e:?}"))
    }

    async fn extract(&self, fit: &Fit, notify: &crate::space::NotifyTx) -> anyhow::Result<bool> {
        extract(&self.f.db, &self.f.world, notify, self.f.owner, self.f.ship, self.f.system, fit, self.module, self.module_type, FLAG_HI_SLOT_FIRST, self.rock).await.map_err(|e| anyhow::anyhow!("extract: {e:?}"))
    }

    async fn snapshot(&self) -> anyhow::Result<(Option<i64>, i64, usize)> {
        let rock = crate::dao::asteroids::lock(&self.f.db, self.f.system, self.rock, self.ore).await?;
        let items: i64 = self.f.db.query_one("SELECT COALESCE(sum(quantity),0)::bigint AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND type_id=$3 AND flag_id=$4", &[&self.f.owner, &self.f.ship, &self.ore, &FLAG_CARGO]).await?.get("n");
        let ledger = crate::dao::mining_ledger::list(&self.f.db, self.f.owner, i64::MIN, i64::MAX).await?.len();
        Ok((rock, items, ledger))
    }

    async fn cleanup(self) {
        state().carry.remove(&self.module);
        state().rocks.remove(&self.rock);
        self.f.world.remove_ball(self.f.system, self.rock).await;
        crate::dao::asteroids::delete(&self.f.db, self.rock).await.unwrap();
        self.f.cleanup().await;
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn stale_module_and_hull_identity_refuse_without_changes() -> anyhow::Result<()> {
    let m = MiningFixture::new(10, 0.25).await;
    let result: anyhow::Result<()> = async {
        let fit = m.fit().await?;
        let (notify, mut direct) = tokio::sync::mpsc::unbounded_channel();
        let (pilot, mut notices) = tokio::sync::mpsc::unbounded_channel();
        m.f.world.register_pilot(m.f.owner, pilot);
        let before = m.snapshot().await?;
        let carry = m.volume * 0.125;
        state().carry.insert(m.module, carry);
        let other_type = m.f.sde.types.iter().find(|t| t.category_id == Some(crate::consts::CATEGORY_MODULE) && t.type_id != m.module_type).unwrap().type_id;
        let changes = [
            "UPDATE game.items SET online=false WHERE item_id=$1",
            "UPDATE game.items SET location_id=location_id+1 WHERE item_id=$1",
            "UPDATE game.items SET flag_id=flag_id+1 WHERE item_id=$1",
            "UPDATE game.items SET owner_id=owner_id+1 WHERE item_id=$1",
            "UPDATE game.items SET singleton=false WHERE item_id=$1",
        ];
        for sql in changes {
            m.f.db.execute(sql, &[&m.module]).await?;
            let rejected = m.extract(&fit, &notify).await;
            m.f.db.execute("UPDATE game.items SET online=true,location_id=$2,flag_id=$3,owner_id=$4,singleton=true WHERE item_id=$1", &[&m.module, &m.f.ship, &FLAG_HI_SLOT_FIRST, &m.f.owner]).await?;
            anyhow::ensure!(!rejected?, "stale module accepted: {sql}");
            anyhow::ensure!(m.snapshot().await? == before && state().carry.get(&m.module).copied() == Some(carry), "stale module changed resources: {sql}");
            anyhow::ensure!(direct.try_recv().is_err() && notices.try_recv().is_err(), "stale module emitted notification: {sql}");
        }
        m.f.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&m.module, &other_type]).await?;
        let type_result = m.extract(&fit, &notify).await;
        m.f.db.execute("UPDATE game.items SET type_id=$2 WHERE item_id=$1", &[&m.module, &m.module_type]).await?;
        anyhow::ensure!(!type_result? && m.snapshot().await? == before && state().carry.get(&m.module).copied() == Some(carry), "changed type accepted");
        m.f.db.execute("UPDATE game.items SET singleton=false WHERE item_id=$1", &[&m.f.ship]).await?;
        let hull_result = m.extract(&fit, &notify).await;
        m.f.db.execute("UPDATE game.items SET singleton=true WHERE item_id=$1", &[&m.f.ship]).await?;
        anyhow::ensure!(!hull_result? && m.snapshot().await? == before && state().carry.get(&m.module).copied() == Some(carry), "invalid hull accepted");
        m.f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&m.module]).await?;
        anyhow::ensure!(!m.extract(&fit, &notify).await? && m.snapshot().await? == before && state().carry.get(&m.module).copied() == Some(carry), "missing module accepted");
        anyhow::ensure!(direct.try_recv().is_err() && notices.try_recv().is_err(), "refusal emitted notification");
        Ok(())
    }.await;
    m.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn fractional_cycles_commit_only_valid_progress() -> anyhow::Result<()> {
    let m = MiningFixture::new(10, 0.25).await;
    let result: anyhow::Result<()> = async {
        let fit = m.fit().await?;
        anyhow::ensure!((fit.value(Key::Module(m.module), ATTR_MINING_AMOUNT) - m.volume * 0.25).abs() < m.volume * 1e-9, "unexpected effective yield");
        let (notify, mut direct) = tokio::sync::mpsc::unbounded_channel();
        let (pilot, mut notices) = tokio::sync::mpsc::unbounded_channel();
        m.f.world.register_pilot(m.f.owner, pilot);
        let before = m.snapshot().await?;
        anyhow::ensure!(m.extract(&fit, &notify).await?, "first fractional cycle refused");
        anyhow::ensure!((state().carry.get(&m.module).copied().unwrap() - m.volume * 0.25).abs() < m.volume * 1e-9, "first carry incorrect");
        m.f.db.execute("UPDATE game.items SET online=false WHERE item_id=$1", &[&m.module]).await?;
        anyhow::ensure!(!m.extract(&fit, &notify).await?, "offline zero-yield cycle accepted");
        m.f.db.execute("UPDATE game.items SET online=true WHERE item_id=$1", &[&m.module]).await?;
        anyhow::ensure!(m.snapshot().await? == before && (state().carry.get(&m.module).copied().unwrap() - m.volume * 0.25).abs() < m.volume * 1e-9, "refusal changed fractional progress");
        for _ in 0..2 { anyhow::ensure!(m.extract(&fit, &notify).await?, "fractional cycle refused"); }
        anyhow::ensure!(m.snapshot().await? == before && notices.try_recv().is_err(), "zero-yield cycle delivered ore");
        let full = crate::dao::items::transfers::insert(&m.f.db, m.ore, m.f.owner, m.f.ship, FLAG_CARGO, 10).await?;
        anyhow::ensure!(!m.extract(&fit, &notify).await?, "full hold accepted");
        anyhow::ensure!((state().carry.get(&m.module).copied().unwrap() - m.volume * 0.75).abs() < m.volume * 1e-9, "full hold consumed carry");
        anyhow::ensure!(direct.try_recv().is_ok() && notices.try_recv().is_err(), "full hold notices incorrect");
        m.f.db.execute("DELETE FROM game.items WHERE item_id=$1", &[&full]).await?;
        anyhow::ensure!(m.extract(&fit, &notify).await?, "whole-unit cycle refused");
        anyhow::ensure!(m.snapshot().await? == (Some(9), 1, before.2 + 1), "whole-unit delivery incorrect");
        anyhow::ensure!(state().carry.get(&m.module).copied().unwrap().abs() < m.volume * 1e-9, "whole-unit carry incorrect");
        anyhow::ensure!(notices.try_recv().map(|n| n.0).ok().as_deref() == Some("OnItemChange") && notices.try_recv().map(|n| n.0).ok().as_deref() == Some("OnOreMined") && notices.try_recv().is_err(), "delivery notices incorrect");
        Ok(())
    }.await;
    m.cleanup().await;
    result
}

async fn wait_for_blocked(client: &tokio_postgres::Client, blocker: i32) -> anyhow::Result<()> {
    tokio::time::timeout(WAIT, async {
        loop {
            let blocked: bool = client.query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))) AS blocked", &[&blocker]).await?.get("blocked");
            if blocked { return Ok::<(), tokio_postgres::Error>(()); }
            tokio::task::yield_now().await;
        }
    }).await.map_err(|_| anyhow::anyhow!("extraction did not block on the ship row"))??;
    Ok(())
}

async fn blocked_extraction(m: &MiningFixture, cancel: bool) -> anyhow::Result<()> {
    let fit = m.fit().await?;
    let (notify, mut direct) = tokio::sync::mpsc::unbounded_channel();
    let (pilot, mut notices) = tokio::sync::mpsc::unbounded_channel();
    m.f.world.register_pilot(m.f.owner, pilot);
    let before = m.snapshot().await?;
    let carry = m.volume * 0.125;
    state().carry.insert(m.module, carry);
    let config = crate::config::tests::test_database().to_pg_config()?;
    let (observer, observer_connection) = config.connect(tokio_postgres::NoTls).await?;
    let observer_task = tokio::spawn(async move { observer_connection.await });
    let (mut client, connection) = config.connect(tokio_postgres::NoTls).await?;
    let connection_task = tokio::spawn(async move { connection.await });
    let blocker: i32 = client.query_one("SELECT pg_backend_pid() AS pid", &[]).await?.get("pid");
    let tx = client.transaction().await?;
    tx.query_one("SELECT item_id FROM game.items WHERE item_id=$1 FOR UPDATE", &[&m.f.ship]).await?;
    let db = m.f.db.clone();
    let world = m.f.world.clone();
    let (owner, ship, system, module, module_type, rock) = (m.f.owner, m.f.ship, m.f.system, m.module, m.module_type, m.rock);
    let mut handle = tokio::spawn(async move {
        extract(&db, &world, &notify, owner, ship, system, &fit, module, module_type, FLAG_HI_SLOT_FIRST, rock).await
    });
    let barrier = wait_for_blocked(&observer, blocker).await;
    let update = if barrier.is_ok() && !cancel {
        tx.execute("UPDATE game.items SET online=false WHERE item_id=$1", &[&m.module]).await.map(|_| ())
    } else { Ok(()) };
    if cancel { handle.abort(); }
    let commit = tx.commit().await;
    let completion = if cancel {
        let _ = handle.await;
        tokio::time::timeout(WAIT, crate::services::market::lock()).await
            .map(|guard| drop(guard)).map_err(|_| anyhow::anyhow!("owned extraction did not finish"))
    } else {
        match tokio::time::timeout(WAIT, &mut handle).await {
            Ok(joined) => joined.map_err(|e| anyhow::anyhow!("extraction task: {e}"))
                .and_then(|r| r.map_err(|e| anyhow::anyhow!("extract: {e:?}")))
                .and_then(|repeat| if repeat { Err(anyhow::anyhow!("stale extraction repeated")) } else { Ok(()) }),
            Err(_) => {
                handle.abort();
                let _ = handle.await;
                let _ = tokio::time::timeout(WAIT, crate::services::market::lock()).await;
                Err(anyhow::anyhow!("extraction did not finish"))
            }
        }
    };
    drop(observer);
    let _ = observer_task.await;
    drop(client);
    let _ = connection_task.await;
    barrier?;
    update?;
    commit?;
    completion?;
    if cancel {
        anyhow::ensure!(m.snapshot().await? == (None, 1, before.2 + 1), "cancelled caller did not deliver exactly once");
        let logs = crate::dao::mining_ledger::list(&m.f.db, m.f.owner, i64::MIN, i64::MAX).await?;
        anyhow::ensure!((logs[0].type_id, logs[0].solar_system_id, logs[0].quantity, logs[0].quantity_wasted) == (m.ore, m.f.system, 1, 0), "cancelled delivery ledger incorrect");
        anyhow::ensure!(!state().rocks.contains_key(&m.rock), "depleted rock remains cached");
        anyhow::ensure!(m.f.world.ball(m.f.system, m.rock).await.is_none(), "depleted rock remains in actor");
        anyhow::ensure!(notices.try_recv().map(|n| n.0).ok().as_deref() == Some("OnItemChange") && notices.try_recv().map(|n| n.0).ok().as_deref() == Some("OnOreMined") && notices.try_recv().is_err(), "cancelled delivery notices incorrect");
    } else {
        anyhow::ensure!(m.snapshot().await? == before && state().carry.get(&m.module).copied() == Some(carry), "stale raced extraction changed resources or carry");
        anyhow::ensure!(direct.try_recv().is_err() && notices.try_recv().is_err(), "stale raced extraction emitted notices");
    }
    Ok(())
}


#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn module_changed_while_ship_locked_refuses_original_intent() -> anyhow::Result<()> {
    let m = MiningFixture::new(10, 1.0).await;
    let result = blocked_extraction(&m, false).await;
    m.cleanup().await;
    result
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn cancelled_caller_still_completes_last_unit_depletion() -> anyhow::Result<()> {
    let m = MiningFixture::new(1, 1.0).await;
    let result = blocked_extraction(&m, true).await;
    m.cleanup().await;
    result
}
