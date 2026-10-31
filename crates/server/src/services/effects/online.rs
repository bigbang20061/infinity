//! Online state changes for fitted modules, mirroring clientDogmaLocation and godma.
//! Reads effect metadata from the SDE and persists online state in game.items.
//! Resource fitting policy is not implemented.
use super::lifecycle::subject_core;
use super::notify::attribute_change;
use crate::client::{filetime_now, Client};
use crate::consts::{ATTR_IS_ONLINE, EFFECT_ONLINE};
use crate::db::{executor::Executor, outbox::{Effect as OutboxEffect, Outbox}};
use crate::messages;
use crate::sde::Sde;
use crate::services::{db_err, notify_error, CallError, CallResult};
use crate::space::NotifyTx;
use marshal::Value;
use tracing::info;

#[derive(Clone)]
pub(in crate::services) struct OnlinePolicy {
    pub owner: i64,
    pub active_ship: Option<i64>,
    pub station: Option<i64>,
    pub notify: NotifyTx,
}

pub(in crate::services) struct OnlineChanged {
    pub module: i64,
    pub online: bool,
    pub char_id: i64,
    pub ship_id: i64,
    pub notify: NotifyTx,
    pub bank_stops: Vec<crate::services::combat::banks::BankStop>,
    pub notify_online: bool,
}

impl OnlinePolicy {
    pub(in crate::services) fn from_client(c: &Client) -> Result<Self, CallError> {
        Ok(Self {
            owner: c.session.int("charid").ok_or_else(|| notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION))?,
            active_ship: c.session.int("shipid"),
            station: c.session.int("stationid"),
            notify: c.notify_tx.clone(),
        })
    }
}

pub(in crate::services) async fn set_online_core<E: From<OnlineChanged> + Send>(
    ex: &(impl Executor + ?Sized), sde: &Sde, policy: &OnlinePolicy, outbox: &mut Outbox<E>,
    ship: i64, item: i64, online: bool,
) -> Result<bool, CallError> {
    let refuse = || notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION);
    if !crate::dao::items::ship::lock_reload_ship(ex, ship).await.map_err(db_err)? { return Err(refuse()); }
    let (hull, module) = subject_core(ex, sde, policy, ship, item, EFFECT_ONLINE as i32, false).await?;
    let was_online = crate::dao::items::dogma::online(ex, ship).await.map_err(db_err)?.iter().any(|entry| entry.item_id == item);
    if was_online == online && online { return Ok(false); }
    let before = if online { None } else { Some(crate::services::combat::banks::snapshot(ex, ship, &[item]).await?) };
    let master = if online { item } else { crate::services::combat::targeting_charges::master_for_member(ex, ship, item).await? };
    let item_stop = if online { None } else { crate::services::combat::banks::capture_stop(item) };
    let changed = was_online != online;
    if changed && crate::dao::items::combat::set_online(ex, item, ship, policy.owner, module.flag_id, module.type_id, hull.type_id, hull.location_id, hull.flag_id, hull.singleton, online).await.map_err(db_err)? == 0 { return Err(refuse()); }
    let mut bank_stops = Vec::new();
    let mut bank_changed = false;
    if let Some(before) = before {
        if crate::services::combat::banks::remove_member(ex, ship, item, master, true).await? {
            bank_changed = true;
            bank_stops = crate::services::combat::banks::changes(ex, ship, before).await?;
            outbox.notify_sender(policy.notify.clone(), "OnWeaponBanksChanged", vec![Value::Int(ship), crate::services::combat::banks(ex, ship).await?]);
        }
    }
    if changed || bank_changed {
        bank_stops.extend(item_stop);
        outbox.operation(OnlineChanged { module: item, online, char_id: policy.owner, ship_id: ship, notify: policy.notify.clone(), bank_stops, notify_online: changed }.into());
    }
    Ok(changed)
}

pub(in crate::services) fn apply_online_changed(change: OnlineChanged) {
    for stop in change.bank_stops { crate::services::combat::banks::apply_stop(stop); }
    if !change.notify_online { return; }
    let now = filetime_now();
    let environment = super::environment::build(change.module, change.char_id, change.ship_id, None, None, EFFECT_ONLINE);
    let effect = super::notify::ship_effect_args(change.module, EFFECT_ONLINE, environment, change.online, now, -1.0, -1, now);
    let attr = attribute_change(change.char_id, Value::Int(change.module), ATTR_IS_ONLINE, Value::Int(change.online as i64), Value::Int(!change.online as i64));
    let _ = change.notify.send(("OnGodmaShipEffect".into(), effect));
    let _ = change.notify.send(("OnModuleAttributeChanges".into(), vec![Value::list([attr])]));
}

/// Change online state for an authorized fitted module, including docked hangar ships.
pub(crate) async fn set_online(c: &mut Client, ship: i64, item_id: i64, online: bool) -> CallResult {
    let policy = OnlinePolicy::from_client(c)?;
    let db = c.db.clone();
    let sde = c.sde.clone();
    db.transaction(
        move |tx, outbox: &mut Outbox<OnlineChanged>| {
            let policy = policy.clone();
            let sde = sde.clone();
            Box::pin(async move { set_online_core(tx, &sde, &policy, outbox, ship, item_id, online).await })
        },
        |changed, outbox| async move {
            for entry in outbox.into_entries() {
                match entry {
                    OutboxEffect::Operation(change) => apply_online_changed(change),
                    OutboxEffect::Notification(notification) => notification.dispatch(&c.world, None)?,
                    OutboxEffect::Event(event) => crate::lua::on_event(event),
                }
            }
            if changed { info!(user = c.login.user_id, "module {item_id} {}", if online { "online" } else { "offline" }); }
            Ok(Value::None)
        },
    ).await
}
