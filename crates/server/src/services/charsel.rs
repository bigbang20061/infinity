//! charUnboundMgr: character selection and creation before a character is
//! chosen (loginCharacterselection/charselData.py, eve/client/script/ui/login).

use super::{keyval, Call, CallError, CallResult};
use crate::client::{short, Client};
use anyhow::Context;
use marshal::Value;
use tracing::{info, warn};

const CHARACTER_SLOTS: i64 = 3;
/// chooseNameSection.py result codes.
const NAME_VALID: i64 = 1;
const NAME_TOO_SHORT: i64 = -1;
const NAME_TOO_LONG: i64 = -2;
const NAME_ILLEGAL_CHARACTER: i64 = -5;
const NAME_TOO_MANY_SPACES: i64 = -6;
const NAME_CONSECUTIVE_SPACES: i64 = -7;
const NAME_UNAVAILABLE: i64 = -101;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

fn arg_text(call: &Call<'_>, i: usize) -> String {
    call.args.get(i).and_then(|v| v.as_text()).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn arg_int(call: &Call<'_>, i: usize) -> Option<i64> {
    call.args.get(i).and_then(|v| v.as_int())
}

fn name_shape_code(name: &str) -> i64 {
    let n = name.chars().count();
    if n < 3 {
        return NAME_TOO_SHORT;
    }
    if n > 37 {
        return NAME_TOO_LONG;
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '\'') {
        return NAME_ILLEGAL_CHARACTER;
    }
    if name.contains("  ") {
        return NAME_CONSECUTIVE_SPACES;
    }
    if name.matches(' ').count() > 2 {
        return NAME_TOO_MANY_SPACES;
    }
    NAME_VALID
}

async fn name_code(c: &Client, name: &str) -> Result<i64, CallError> {
    let code = name_shape_code(name);
    if code != NAME_VALID {
        return Ok(code);
    }
    let taken = c
        .db
        .client
        .query_opt("SELECT 1 FROM game.characters WHERE lower(name) = lower($1) AND NOT deleted", &[&name])
        .await
        .map_err(db_err)?
        .is_some();
    Ok(if taken { NAME_UNAVAILABLE } else { NAME_VALID })
}

/// (school corporation, station, solar system, constellation, region) for a new character.
async fn starting_location(c: &Client, school_id: Option<i64>, race_id: i64) -> Result<(i64, i64, i64, i64, i64), CallError> {
    let row = c
        .db
        .client
        .query_opt(
            "WITH school AS (
                 SELECT data FROM sde_native.schools
                 WHERE ($1::bigint IS NOT NULL AND key = $1::bigint::text)
                    OR ($1::bigint IS NULL AND (data->>'raceID')::int = $2::int)
                 ORDER BY key::int LIMIT 1)
             SELECT (school.data->>'corporationID')::bigint, st.station_id::bigint, st.solar_system_id::bigint,
                    st.constellation_id::bigint, st.region_id::bigint
             FROM school, jsonb_array_elements_text(school.data->'startingStations') s(id)
             JOIN sde.sta_stations st ON st.station_id = s.id::int
             ORDER BY random() LIMIT 1",
            &[&school_id, &(race_id as i32)],
        )
        .await
        .map_err(db_err)?
        .context("no starting station for school/race")?;
    Ok((row.get(0), row.get(1), row.get(2), row.get(3), row.get(4)))
}

async fn character_rows(c: &Client) -> Result<Vec<Value>, CallError> {
    let rows = c
        .db
        .client
        .query(
            "SELECT ch.character_id, ch.name, ch.corporation_id, ch.station_id, ch.solar_system_id, ch.balance::float8, ch.skill_points,
                    (extract(epoch FROM ch.logoff_at) * 10000000)::bigint + 116444736000000000,
                    ch.gender_id, ch.bloodline_id, ch.race_id, ship.type_id, ch.delete_prepare_at
             FROM game.characters ch LEFT JOIN game.items ship ON ship.item_id = ch.ship_id
             WHERE ch.user_id = $1 AND NOT ch.deleted ORDER BY ch.character_id",
            &[&c.login.user_id],
        )
        .await
        .map_err(db_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            keyval([
                ("characterID", Value::Int(r.get(0))),
                ("characterName", Value::unicode(r.get::<_, String>(1))),
                ("corporationID", Value::Int(r.get(2))),
                ("allianceID", Value::None),
                ("factionID", Value::None),
                ("stationID", r.get::<_, Option<i64>>(3).map(Value::Int).unwrap_or(Value::None)),
                ("solarSystemID", r.get::<_, Option<i64>>(4).map(Value::Int).unwrap_or(Value::None)),
                ("structureID", Value::None),
                ("balance", Value::Float(r.get(5))),
                ("skillPoints", Value::Int(r.get(6))),
                ("logoffDate", Value::Int(r.get(7))),
                ("deletePrepareDateTime", r.get::<_, Option<i64>>(12).map(Value::Int).unwrap_or(Value::None)),
                ("genderID", Value::Int(r.get::<_, i32>(8) as i64)),
                ("bloodlineID", Value::Int(r.get::<_, i32>(9) as i64)),
                ("raceID", Value::Int(r.get::<_, i32>(10) as i64)),
                ("shipTypeID", r.get::<_, Option<i32>>(11).map(|t| Value::Int(t as i64)).unwrap_or(Value::Int(606))),
                ("gender", Value::Int(r.get::<_, i32>(8) as i64)),
                ("balanceChange", Value::Float(0.0)),
                ("paperdollState", Value::Int(0)),
                // Skill training summary: nothing in training.
                ("skillTypeID", Value::None),
                ("toLevel", Value::None),
                ("trainingStartTime", Value::None),
                ("trainingEndTime", Value::None),
                ("queueEndTime", Value::None),
                ("finishSP", Value::None),
                ("trainedSP", Value::None),
                ("finishedSkills", Value::list([])),
                ("unreadMailCount", Value::Int(0)),
                ("unprocessedNotifications", Value::Int(0)),
                ("lockTypeID", Value::None),
            ])
        })
        .collect())
}

/// A character submitted to the biomass queue can be deleted after this long (retail
/// waits hours; this server only guards against a stray click).
const BIOMASS_DELAY_SECS: i64 = 30;

/// userSvc.UserLogOffCharacter(): back to character selection. The character leaves the
/// game as on a disconnect and the session drops everything about it.
pub async fn log_off(c: &mut Client) -> CallResult {
    let Some(char_id) = c.session.int("charid") else { return Ok(Value::Bool(true)) };
    super::combat::reset(c);
    c.leave_world().await.map_err(CallError::Internal)?;
    c.db.client.execute("UPDATE game.characters SET logoff_at = now() WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?;
    let cleared = [
        "charid", "corpid", "corprole", "rolesAtAll", "rolesAtBase", "rolesAtHQ", "rolesAtOther", "corpAccountKey", "allianceid", "warfactionid",
        "genderID", "bloodlineID", "raceID", "hqID", "stationid", "solarsystemid", "solarsystemid2", "constellationid", "regionid", "shipid",
    ];
    c.change_session(cleared.into_iter().map(|k| (k, Value::None)).collect());
    info!(user = c.login.user_id, "character {char_id} logged off to character selection");
    Ok(Value::Bool(true))
}

/// A character of this account that isn't deleted.
async fn own_character(c: &Client, call: &Call<'_>) -> Result<i64, CallError> {
    let char_id = arg_int(call, 0).unwrap_or(0);
    c.db.client
        .query_opt("SELECT 1 FROM game.characters WHERE character_id = $1 AND user_id = $2 AND NOT deleted", &[&char_id, &c.login.user_id])
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: "CharacterNotFound".into(), dict: vec![] })?;
    Ok(char_id)
}

/// inventory flags (inventorycommon.const)
pub const FLAG_HANGAR: i32 = 4;
pub const FLAG_PILOT: i32 = 57;

/// Give the character a rookie ship in their station hangar if they have none.
async fn ensure_ship(c: &Client, char_id: i64, race_id: i32, station_id: i64) -> Result<i64, CallError> {
    if let Some(r) = c
        .db
        .client
        .query_opt(
            "SELECT ship_id FROM game.characters ch JOIN game.items i ON i.item_id = ch.ship_id WHERE ch.character_id = $1",
            &[&char_id],
        )
        .await
        .map_err(db_err)?
    {
        return Ok(r.get(0));
    }
    let ship_type: i32 = c
        .db
        .client
        .query_opt("SELECT ship_type_id FROM sde.chr_races WHERE race_id = $1 AND ship_type_id IS NOT NULL", &[&race_id])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(606); // Velator
    let ship_id: i64 = c
        .db
        .client
        .query_one(
            "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton)
             VALUES ($1, $2, $3, $4, 1, true) RETURNING item_id",
            &[&ship_type, &char_id, &station_id, &FLAG_HANGAR],
        )
        .await
        .map_err(db_err)?
        .get(0);
    c.db.client.execute("UPDATE game.characters SET ship_id = $2 WHERE character_id = $1", &[&char_id, &ship_id]).await.map_err(db_err)?;
    info!(user = c.login.user_id, "issued rookie ship {ship_id} (type {ship_type}) to character {char_id}");
    Ok(ship_id)
}

/// Push the docked-in-station session for `char_id` (eveSessions.GetSessionValuesFromRowset).
async fn select_character(c: &mut Client, char_id: i64) -> Result<(), CallError> {
    let r = c
        .db
        .client
        .query_opt(
            "SELECT corporation_id, station_id, solar_system_id, constellation_id, region_id, gender_id, bloodline_id, race_id, name, corp_roles
             FROM game.characters WHERE character_id = $1 AND user_id = $2 AND NOT deleted",
            &[&char_id, &c.login.user_id],
        )
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: "CharacterNotFound".into(), dict: vec![] })?;
    let station: Option<i64> = r.get(1);
    let system: i64 = r.get(2);
    let race: i32 = r.get(7);
    // A character that logged off in space resumes in its ship at the stored position.
    let space_ship = c
        .db
        .client
        .query_opt(
            "SELECT i.item_id, i.x, i.y, i.z FROM game.characters ch JOIN game.items i ON i.item_id = ch.ship_id
             WHERE ch.character_id = $1 AND i.location_id = ch.solar_system_id AND i.x IS NOT NULL",
            &[&char_id],
        )
        .await
        .map_err(db_err)?;
    let ship = match (&space_ship, station) {
        (Some(s), None) => s.get::<_, i64>(0),
        (_, Some(st)) => ensure_ship(c, char_id, race, st).await?,
        (None, None) => return Err(CallError::Internal(anyhow::anyhow!("character {char_id} is in space without a ship"))),
    };
    let has_skills = c
        .db
        .client
        .query_opt("SELECT 1 FROM game.skills WHERE character_id = $1 LIMIT 1", &[&char_id])
        .await
        .map_err(db_err)?
        .is_some();
    if !has_skills {
        super::skills::grant_starting_skills(c, char_id, race).await?;
    }
    c.db.client.execute("UPDATE game.characters SET logoff_at = now() WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?;
    let int = Value::Int;
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let corp: i64 = r.get(0);
    let roles: i64 = r.get(9);
    c.change_session(vec![
        ("charid", int(char_id)),
        ("corpid", int(corp)),
        ("corprole", int(roles)),
        ("rolesAtAll", int(roles)),
        ("rolesAtBase", int(roles)),
        ("rolesAtHQ", int(roles)),
        ("rolesAtOther", int(roles)),
        ("corpAccountKey", int(1000)),
        ("genderID", int(r.get::<_, i32>(5) as i64)),
        ("bloodlineID", int(r.get::<_, i32>(6) as i64)),
        ("raceID", int(race as i64)),
        ("hqID", opt(station)),
        ("stationid", opt(station)),
        ("solarsystemid", if station.is_none() { int(system) } else { Value::None }),
        ("solarsystemid2", int(system)),
        ("constellationid", int(r.get(3))),
        ("regionid", int(r.get(4))),
        ("shipid", int(ship)),
    ]);
    if station.is_none() {
        let pos = space_ship.as_ref().map(|s| [s.get::<_, f64>(1), s.get::<_, f64>(2), s.get::<_, f64>(3)]).unwrap_or([0.0; 3]);
        let ball = super::ship::ship_ball(c, ship, pos).await?;
        c.world.add_ship(system, ball).await.map_err(CallError::Internal)?;
    }
    c.world.register_pilot(char_id, c.notify_tx.clone());
    let role = c.session.int("role").unwrap_or(0);
    c.chat.set_profile(char_id, &r.get::<_, String>(8), r.get::<_, i32>(5) as i64, corp, None, None, role);
    info!(user = c.login.user_id, "character {char_id} entering game at station {station:?} / system {system}");
    Ok(())
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> CallResult {
    match call.method {
        // (userDetails, trainingDetails, characterDetails, wars)
        "GetCharacterSelectionData" => {
            let created: i64 = c
                .db
                .client
                .query_one(
                    "SELECT (extract(epoch FROM created_at) * 10000000)::bigint + 116444736000000000 FROM game.accounts WHERE user_id = $1",
                    &[&c.login.user_id],
                )
                .await
                .map_err(db_err)?
                .get(0);
            let user = keyval([
                ("characterSlots", Value::Int(CHARACTER_SLOTS)),
                ("maxCharacterSlots", Value::Int(CHARACTER_SLOTS)),
                ("userName", Value::unicode(c.login.user_name.clone())),
                ("creationDate", Value::Int(created)),
                ("subscriptionEndTime", Value::Int(super::FAR_FUTURE)),
            ]);
            Ok(Value::tuple([
                Value::list([user]),
                Value::tuple([Value::None, Value::None]),
                Value::List(character_rows(c).await?),
                Value::list([]),
            ]))
        }
        "GetNumCharacters" => {
            let n: i64 = c
                .db
                .client
                .query_one("SELECT count(*) FROM game.characters WHERE user_id = $1 AND NOT deleted", &[&c.login.user_id])
                .await
                .map_err(db_err)?
                .get(0);
            Ok(Value::Int(n))
        }
        "GetCharacterLockType" => Ok(Value::None),
        // PrepareCharacterForDelete(charID) -> when it can be deleted (FILETIME).
        "PrepareCharacterForDelete" => {
            let char_id = own_character(c, call).await?;
            let when = crate::client::filetime_now() + BIOMASS_DELAY_SECS * 10_000_000;
            c.db.client.execute("UPDATE game.characters SET delete_prepare_at = $2 WHERE character_id = $1", &[&char_id, &when]).await.map_err(db_err)?;
            info!(user = c.login.user_id, "character {char_id} submitted to the biomass queue");
            Ok(Value::Int(when))
        }
        "CancelCharacterDeletePrepare" => {
            let char_id = own_character(c, call).await?;
            c.db.client.execute("UPDATE game.characters SET delete_prepare_at = NULL WHERE character_id = $1", &[&char_id]).await.map_err(db_err)?;
            Ok(Value::None)
        }
        // DeleteCharacter(charID) -> an error message key, or None once deleted.
        "DeleteCharacter" => {
            let char_id = own_character(c, call).await?;
            let deleted = c
                .db
                .client
                .execute(
                    "UPDATE game.characters SET deleted = true WHERE character_id = $1 AND delete_prepare_at IS NOT NULL AND delete_prepare_at <= $2",
                    &[&char_id, &crate::client::filetime_now()],
                )
                .await
                .map_err(db_err)?;
            if deleted == 0 {
                return Ok(Value::str("CharacterNotReadyForDeletion"));
            }
            info!(user = c.login.user_id, "character {char_id} deleted");
            Ok(Value::None)
        }
        "GetCharOmegaDowngradeStatus" => Ok(Value::Bool(false)),
        "SelectCharacterID" => {
            let char_id = call.args.first().and_then(|v| v.as_int()).or_else(|| call.kwargs.get("charID").and_then(|v| v.as_int()));
            let char_id = char_id.ok_or_else(|| CallError::Internal(anyhow::anyhow!("SelectCharacterID without charID")))?;
            select_character(c, char_id).await?;
            Ok(Value::None)
        }
        // gatekeeper cohorts (feature flags) the user belongs to.
        "GetCohortsForUser" => Ok(Value::list([])),

        "ValidateNameEx" => Ok(Value::Int(name_code(c, &arg_text(call, 0)).await?)),
        "GetValidRandomName" => {
            const FIRST: [&str; 12] = ["Aren", "Kael", "Sora", "Veyra", "Toma", "Ilsa", "Daro", "Mira", "Lukas", "Nadia", "Joren", "Tessa"];
            const LAST: [&str; 12] = ["Vale", "Okaro", "Thorne", "Kassen", "Ruun", "Halvik", "Moreau", "Sandor", "Ikonen", "Drake", "Arvel", "Castor"];
            for _ in 0..50 {
                let r = crate::handshake::rand_i64() as usize;
                let name = format!("{} {} {}", FIRST[r % 12], LAST[(r / 12) % 12], 100 + (r / 144) % 900);
                if name_code(c, &name).await? == NAME_VALID {
                    return Ok(Value::unicode(name));
                }
            }
            Err(CallError::Internal(anyhow::anyhow!("could not generate a free random name")))
        }

        // (name, raceID, bloodlineID, genderID, ancestryID, appearance, portrait, schoolID, ?, qaStarterSystemID)
        "CreateCharacterWithDoll" => {
            let name = arg_text(call, 0);
            let code = name_code(c, &name).await?;
            if code != NAME_VALID {
                return Err(CallError::User { msg: "CharNameInvalid".into(), dict: vec![] });
            }
            let race_id = arg_int(call, 1).unwrap_or(1);
            let bloodline_id = arg_int(call, 2).unwrap_or(1);
            let gender_id = arg_int(call, 3).unwrap_or(0);
            let ancestry_id = arg_int(call, 4);
            let school_id = arg_int(call, 7);
            let appearance = marshal::dumps(call.args.get(5).unwrap_or(&Value::None));
            let portrait = marshal::dumps(call.args.get(6).unwrap_or(&Value::None));
            let (corp, station, system, constellation, region) = starting_location(c, school_id, race_id).await?;
            let row = c
                .db
                .client
                .query_one(
                    "INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, ancestry_id, school_id,
                         corporation_id, station_id, solar_system_id, constellation_id, region_id, balance, appearance, portrait, home_station_id)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 5000000, $13, $14, $9) RETURNING character_id",
                    &[
                        &c.login.user_id,
                        &name,
                        &(race_id as i32),
                        &(bloodline_id as i32),
                        &(gender_id as i32),
                        &ancestry_id.map(|v| v as i32),
                        &school_id.map(|v| v as i32),
                        &corp,
                        &station,
                        &system,
                        &constellation,
                        &region,
                        &appearance,
                        &portrait,
                    ],
                )
                .await
                .map_err(db_err)?;
            let char_id: i64 = row.get(0);
            super::skills::grant_starting_skills(c, char_id, race_id as i32).await?;
            info!(user = c.login.user_id, "created character {name:?} ({char_id}) in station {station}");
            Ok(Value::Int(char_id))
        }
        _ => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED charUnboundMgr::{} args={} kwargs={}",
                call.method,
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
    }
}
