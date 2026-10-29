//! Services the client calls while entering / sitting in a station that only
//! need empty-but-well-formed answers for now (no wars, agents, campaigns...).

use super::{rowset, Call, CallError, CallResult};
use crate::client::Client;
use marshal::Value;

/// CPython 2.7 `marshal.dumps(code)` for `def SettingsInfo(): return {}`.
/// carbonui settingsSvc.UpdateSettingsStatistics loads it into a function and calls it.
fn settings_info_code() -> Vec<u8> {
    fn i32le(out: &mut Vec<u8>, v: i32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn s(out: &mut Vec<u8>, b: &[u8]) {
        out.push(b's');
        i32le(out, b.len() as i32);
        out.extend_from_slice(b);
    }
    fn empty_tuple(out: &mut Vec<u8>) {
        out.push(b'(');
        i32le(out, 0);
    }
    let mut o = vec![b'c'];
    i32le(&mut o, 0); // argcount
    i32le(&mut o, 0); // nlocals
    i32le(&mut o, 1); // stacksize
    i32le(&mut o, 0x43); // CO_OPTIMIZED | CO_NEWLOCALS | CO_NOFREE
    s(&mut o, &[0x69, 0x00, 0x00, 0x53]); // BUILD_MAP 0; RETURN_VALUE
    o.push(b'(');
    i32le(&mut o, 1);
    o.push(b'N'); // consts = (None,)
    empty_tuple(&mut o); // names
    empty_tuple(&mut o); // varnames
    empty_tuple(&mut o); // freevars
    empty_tuple(&mut o); // cellvars
    s(&mut o, b"infinity-emu"); // filename
    s(&mut o, b"SettingsInfo"); // name
    i32le(&mut o, 1); // firstlineno
    s(&mut o, b""); // lnotab
    o
}

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

/// Every NPC station as a rowset; the client indexes it by stationID (uisvc.GetStation).
async fn station_info(c: &Client) -> Result<Value, CallError> {
    let rows = c
        .db
        .client
        .query(
            "SELECT station_id::bigint, type_id::bigint, owner_id::bigint, solar_system_id::bigint, constellation_id::bigint,
                    region_id::bigint, operation_id::bigint, x, y, z, security
             FROM sde.sta_stations ORDER BY station_id",
            &[],
        )
        .await
        .map_err(db_err)?;
    let lines = rows
        .iter()
        .map(|r| {
            let mut l: Vec<Value> = (0..7).map(|i| r.get::<_, Option<i64>>(i).map(Value::Int).unwrap_or(Value::None)).collect();
            l.extend((7..11).map(|i| Value::Float(r.get::<_, Option<f64>>(i).unwrap_or(0.0))));
            l.insert(1, Value::unicode(""));
            l
        })
        .collect();
    Ok(rowset(
        &["stationID", "stationName", "stationTypeID", "ownerID", "solarSystemID", "constellationID", "regionID", "operationID", "x", "y", "z", "security"],
        lines,
    ))
}

/// `Some(reply)` for calls this module answers, `None` to fall through.
pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let int = Value::Int;
    let empty_dict = || Value::Dict(vec![]);
    Some(Ok(match (call.service, call.method) {
        ("seasonManager", "get_season_data_for_character") => Value::None,
        // (jumpActivation, jumpFatigue, lastUpdated)
        ("jumpTimers", "GetTimers") => Value::tuple([int(0), int(0), int(crate::client::filetime_now())]),
        ("petitioner", "GetUnreadMessages") => Value::list([]),
        // No external help centre: the help window keeps the in-game petition flow.
        ("petitioner", "IsZendeskEnabled") | ("petitioner", "IsZendeskSwapEnabled") => Value::Bool(false),
        ("ship", "GetDirtTimestamp") => int(crate::client::filetime_now()),
        ("shipKillCounter", "GetItemKillCountPlayer") => int(0),
        ("shipCosmeticsMgr", "GetEnabledCosmetics") => empty_dict(),
        ("keeper", "GetCurrentDungeonForCharacter") => Value::None,
        ("dogmaIM", "GetTargets") | ("dogmaIM", "GetTargeters") => Value::list([]),
        ("scanMgr", "SignalTrackerRegister") => Value::None,
        ("monolithTaskTrackerMgr", "HasEverWarped") => Value::Bool(true),
        ("essMgr", "GetDataForClientSolarSystem") => Value::None,
        ("bountyProxy", "GetMyKillRights") => Value::list([]),
        // (bounties [(targetID, bounty)], killRights)
        ("bountyProxy", "GetBountiesAndKillRights") => Value::tuple([Value::list([]), Value::list([])]),
        // (solarSystemID, campaignSnapshot): no insurgency here. The system must match the
        // session's or the client asks again.
        ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot") => Value::tuple([c.session.get("solarsystemid2").clone(), Value::None]),
        ("userSvc", "GetMultiCharactersTrainingSlots") => empty_dict(),
        ("nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections") => int(0),
        ("securityMgr", "get_modified_systems") => empty_dict(),
        ("monolithTaskTrackerMgr", "GetCompletedAchievementsAndClientEventCount") => {
            Value::dict([("completedDict", empty_dict()), ("eventDict", empty_dict())])
        }
        // (fightersInTubes, fightersInSpace, abilityStates)
        ("fighterMgr", "GetFightersForShip") => Value::tuple([Value::list([]), Value::list([]), empty_dict()]),
        // Placebo-signed CPython code object: (marshalled code, 0)
        ("charMgr", "GetSettingsInfo") => Value::tuple([Value::str(settings_info_code()), int(0)]),
        ("charMgr", "LogSettings") => Value::None,
        ("charMgr", "GetCharacterSettings") => empty_dict(),
        ("ProjectDiscovery", "initialize_tutorial_status") => Value::Bool(true),
        // Analysis Kredits (the LP store's Project Discovery currency).
        ("ProjectDiscovery", "get_player_analysis_kredits") => int(0),
        // Store telemetry/refresh requests; the live server answers with pushed data we don't model.
        ("monetizationMgr", "CallUserRequested") | ("exchangeShopMgr", "CallExchangeShopOpened") => Value::None,
        // Moving ISK into the amplification matrix isn't modelled: refuse rather than let the
        // client report a deposit that never happened.
        ("amplificationMatrixSolarsystemMgr", "TransferCash") => {
            return Some(Err(CallError::User {
                msg: "CustomNotify".into(),
                dict: vec![("notify".into(), Value::unicode("增幅矩阵暂不可用。"))],
            }))
        }
        ("milestoneMgr", "ProcessCharacterLogon") => Value::None,
        ("structureDirectory", "GetMyDockableStructures") => Value::list([]),
        ("officeManager", "GetMyCorporationsOffices") => Value::list([]),
        ("fwWarzoneSolarsystem", "GetLocalOccupationState") => Value::tuple([c.session.get("solarsystemid2").clone(), Value::None]),
        ("map", "GetStationInfo") => match station_info(c).await {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        },
        ("activityMgr", "GetActivityBadgeStatuses") => Value::list([]),
        ("warRegistry", "GetWars") => empty_dict(),
        ("operationsManager", "can_character_play_the_tutorial") => Value::Bool(false),
        ("insurgencySolarsystem", "GetAllVisibleCampaigns") => Value::list([]),
        ("battlepassSeasonMgr", "CallSeasonDataRequested") => Value::None,
        ("shipAdaptionMgr", "fetch_missions") => Value::None,
        ("clientStatLogger", _) => Value::None,
        // [ownerID, itemID, operationID, stationTypeID]
        ("stationSvc", "GetStationItemBits") => {
            let station = c.session.int("stationid").unwrap_or(0);
            let r = match c
                .db
                .client
                .query_one(
                    "SELECT owner_id::bigint, station_id::bigint, operation_id::bigint, type_id::bigint FROM sde.sta_stations WHERE station_id = $1",
                    &[&(station as i32)],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => return Some(Err(db_err(e))),
            };
            Value::list((0..4).map(|i| r.get::<_, Option<i64>>(i).map(int).unwrap_or(Value::None)))
        }
        _ => return None,
    }))
}

