//! Full kill-report rows for Crimewatch and `eve/client/script/ui/shared/neocom/war/warReport.py`.
//! Reads game.killmails and immutable game.killmail_wars; old alliance snapshots remain unknown.
//! Faction affiliation and historical membership reconstruction are unsupported.

use super::{db_err, keyval, CallResult, Client, Value};
use crate::dao::killmails::{self as dao, Killmail};
use std::collections::HashMap;

pub(crate) fn minimum_wars(projections: &[dao::wars::Projection]) -> HashMap<i64, i64> {
    let mut result = HashMap::new();
    for p in projections {
        result.entry(p.kill_id).and_modify(|id: &mut i64| *id = (*id).min(p.war_id)).or_insert(p.war_id);
    }
    result
}

pub(super) async fn killmails(c: &Client, kills: Option<bool>, key: i64, num: i64, before: Option<i64>) -> CallResult {
    let filter = match kills {
        Some(true) => dao::Filter::Kills,
        Some(false) => dao::Filter::Losses,
        None => dao::Filter::Id,
    };
    let rows = dao::list(&c.db, filter, key, before, num.clamp(1, 500)).await.map_err(db_err)?;
    if rows.is_empty() { return Ok(Value::list([])); }
    let ids: Vec<_> = rows.iter().map(|row| row.kill_id).collect();
    let projections = dao::wars::by_kills(&c.db, &ids).await.map_err(db_err)?;
    let wars = minimum_wars(&projections);
    Ok(Value::list(rows.into_iter().map(|row| {
        let war_id = wars.get(&row.kill_id).copied();
        killmail_value(row, war_id)
    })))
}

pub(crate) fn killmail_value(r: Killmail, war_id: Option<i64>) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    keyval([
        ("killID", Value::Int(r.kill_id)),
        ("killTime", Value::Int(r.kill_time)),
        ("solarSystemID", Value::Int(r.solar_system_id)),
        ("moonID", Value::None),
        ("victimCharacterID", Value::Int(r.victim_id)),
        ("victimCorporationID", Value::Int(r.victim_corp_id)),
        ("victimAllianceID", opt(r.victim_alliance_id)),
        ("victimFactionID", Value::None),
        ("victimShipTypeID", Value::Int(r.victim_ship_type as i64)),
        ("victimDamageTaken", Value::Int(r.damage_taken.round() as i64)),
        ("finalCharacterID", opt(r.final_char_id)),
        ("finalCorporationID", opt(r.final_corp_id)),
        ("finalAllianceID", opt(r.final_alliance_id)),
        ("finalFactionID", Value::None),
        ("finalShipTypeID", Value::Int(r.final_ship_type as i64)),
        ("finalWeaponTypeID", Value::Int(r.final_weapon_type as i64)),
        ("finalDamageDone", Value::Int(r.final_damage.round() as i64)),
        ("finalSecurityStatus", Value::Float(r.final_sec_status)),
        ("iskLost", Value::Float(r.isk_lost)),
        ("warID", opt(war_id)),
        ("killBlob", Value::unicode(r.blob)),
    ])
}
