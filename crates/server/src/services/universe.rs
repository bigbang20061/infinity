//! Map, corporation and wallet-side services the neocom windows load from
//! (eve/client/script/ui/shared/maps, mapView, neocom/corporation, wallet).

use super::{db_err, row, rowset, Call, CallError, CallResult};
use crate::client::Client;
use crate::consts::{CORPORATION_TYPE_NPC, CORPORATION_UNLIMITED_MEMBER_LIMIT, CORP_DIVISION_NAME_COUNT, FACTION_AMARR, FACTION_CALDARI, FACTION_GALLENTE, FACTION_MINMATAR};
use marshal::Value;

/// Faction warfare warzones: (warzoneID, the two factions fighting over it).
const WARZONES: &[(i64, [i64; 2])] = &[
    (1, [FACTION_AMARR, FACTION_MINMATAR]), // Amarr Empire / Minmatar Republic
    (2, [FACTION_CALDARI, FACTION_GALLENTE]), // Caldari State / Gallente Federation
];

/// Server default NPC corporation shares in public corporation information.
const NPC_CORPORATION_SHARES: i64 = 1000;
/// Client corporation row: optional logo shape, colour and typeface fields.
const CORP_LOGO_FIELDS: usize = 7;
/// Server default industry cost modifier for each solar system.
const INDUSTRY_COST_MODIFIER: f64 = 0.001;
/// Server default maximum dynamic resource output.
const DYNAMIC_RESOURCE_MAX_OUTPUT: f64 = 2.0;

/// Public corporation row consumed by eve/client/script/ui/shared/neocom/corporation.
const CORP_HEADER: &[&str] = &[
    "corporationID",
    "corporationName",
    "description",
    "tickerName",
    "url",
    "taxRate",
    "loyaltyPointTaxRate",
    "minimumJoinStanding",
    "corporationType",
    "hasPlayerPersonnelManager",
    "sendCharTerminationMessage",
    "creatorID",
    "ceoID",
    "stationID",
    "raceID",
    "allianceID",
    "warFactionID",
    "shares",
    "memberCount",
    "memberLimit",
    "allowedMemberRaceIDs",
    "graphicID",
    "shape1",
    "shape2",
    "shape3",
    "color1",
    "color2",
    "color3",
    "typeface",
    "division1",
    "division2",
    "division3",
    "division4",
    "division5",
    "division6",
    "division7",
    "walletDivision1",
    "walletDivision2",
    "walletDivision3",
    "walletDivision4",
    "walletDivision5",
    "walletDivision6",
    "walletDivision7",
    "deleted",
    "isRecruiting",
    "friendlyFire",
    "aggressionSettings",
    "createDateTime",
    "chosenExecutorID",
    "allowWar",
];

/// A corporation row: NPC corporations from the SDE, player corporations from game.corporations.
async fn corporation(c: &Client, corp_id: i64) -> Result<Value, CallError> {
    let r = c
        .db
        .client
        .query_opt(
            "SELECT name, COALESCE(data->'description'->>'en', ''), ticker, tax_rate, ceo_id::bigint, station_id::bigint,
                    race_id::bigint, faction_id::bigint, COALESCE(member_limit::bigint, $2::bigint),
                    (SELECT count(*) FROM game.characters ch WHERE ch.corporation_id = $1::bigint AND NOT ch.deleted)
             FROM sde.crp_npc_corporations WHERE corporation_id = $1::bigint",
            &[&corp_id, &CORPORATION_UNLIMITED_MEMBER_LIMIT],
        )
        .await
        .map_err(db_err)?;
    let Some(r) = r else {
        return Ok(super::corps::corporation_line(&c.db, corp_id).await?.map(|line| row(Value::list(CORP_HEADER.iter().map(|h| Value::str(*h))), line)).unwrap_or(Value::None));
    };
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    let s = |v: Option<String>| Value::unicode(v.unwrap_or_default());
    let mut line = vec![
        Value::Int(corp_id),
        s(r.get(0)),
        s(r.get(1)),
        s(r.get(2)),
        Value::unicode(""),
        Value::Float(r.get::<_, Option<f64>>(3).unwrap_or(0.0)),
        Value::Float(0.0),
        Value::Float(0.0),
        Value::Int(CORPORATION_TYPE_NPC),
        Value::Bool(false),
        Value::Bool(true),
        opt(r.get(4)),
        opt(r.get(4)),
        opt(r.get(5)),
        opt(r.get(6)),
        Value::None,
        opt(r.get(7)),
        Value::Int(NPC_CORPORATION_SHARES),
        Value::Int(r.get::<_, i64>(9)),
        Value::Int(r.get::<_, i64>(8)),
        Value::None,
        Value::None,
    ];
    line.extend(std::iter::repeat_n(Value::None, CORP_LOGO_FIELDS)); // shapes, colours, typeface
    line.extend(std::iter::repeat_n(Value::None, CORP_DIVISION_NAME_COUNT)); // division names
    // ..., chosenExecutorID, allowWar (no wars can be declared on NPC corporations)
    line.extend([Value::Bool(false), Value::Bool(false), Value::Bool(false), Value::None, Value::None, Value::None, Value::Bool(false)]);
    Ok(row(Value::list(CORP_HEADER.iter().map(|h| Value::str(*h))), line))
}

async fn station_counts(c: &Client) -> Result<Value, CallError> {
    let rows = c
        .db
        .client
        .query("SELECT solar_system_id::bigint, count(*) FROM sde.sta_stations GROUP BY solar_system_id", &[])
        .await
        .map_err(db_err)?;
    Ok(Value::List(rows.iter().map(|r| Value::tuple([Value::Int(r.get(0)), Value::Int(r.get(1))])).collect()))
}

async fn industry_cost_modifiers(c: &Client) -> Result<Value, CallError> {
    let rows = c.db.client.query("SELECT solar_system_id::bigint FROM sde.map_solar_systems", &[]).await.map_err(db_err)?;
    Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.get(0)), Value::Float(INDUSTRY_COST_MODIFIER))).collect()))
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let my_corp = c.session.int("corpid").unwrap_or(0);
    let arg_int = |i: usize| call.args.get(i).and_then(|v| v.as_int());
    let history_header = ["solarSystemID", "value1", "value2", "value3"];
    Some(Ok(match (call.service, call.method) {
        // ---- map ----
        ("map", "GetHistory") => rowset(&history_header, vec![]),
        ("map", "GetStationCount") => return Some(station_counts(c).await),
        ("map", "GetSolarSystemVisits") => Value::list([]),
        ("map", "GetMyExtraMapInfo") => Value::None,
        ("map", "GetMyExtraMapInfoAgents") => rowset(&["fromID", "rank"], vec![]),
        ("map", "GetDeadspaceComplexMap") | ("map", "GetDeadspaceAgentsMap") => Value::None,
        ("map", "GetBeaconCount") => Value::Dict(vec![]),
        ("map", "GetIndustryJobsOverLast") => rowset(&history_header, vec![]),
        ("map", "GetIndustryCostModifier") => return Some(industry_cost_modifiers(c).await),
        ("map", "GetInsurgencySystem") => Value::None,
        ("map", "GetAllRoamingWeatherSystems") => Value::Dict(vec![]),
        ("map", "GetTriglavianMinorVictorySystems") | ("map", "GetEdencomMinorVictorySystems") | ("map", "GetEdencomFortressSystems") => {
            Value::list([])
        }
        ("stationSvc", "GetAllianceSystems") => Value::list([]),
        // (jumpBridgeGates, hasAccessTo, hasNoAccessTo)
        ("structureDirectory", "GetStructureMapData") => Value::list([]),
        // SKIN licences the character has activated: none yet.
        ("shipCosmeticsMgr", "GetLicencedSkins") | ("shipCosmeticsMgr", "GetLicencedSkinsForShipType") => Value::list([]),
        ("structureDirectory", "GetJumpBridgesWithMyAccess") => Value::tuple([Value::list([]), Value::list([]), Value::list([])]),
        ("dynamicResourceCacheMgr", "GetDBSMapData") => Value::Dict(vec![]),
        // Bounty output range and its equilibrium (mapFilterDynamicBounty reads all three).
        ("dynamicResourceCacheMgr", "GetDynamicResourceSettings") => Value::dict([
            ("minOutput", Value::Float(0.0)),
            ("maxOutput", Value::Float(DYNAMIC_RESOURCE_MAX_OUTPUT)),
            ("equilibriumValue", Value::Float(1.0)),
        ]),
        ("structureDirectory", "GetSolarSystemsWithBeacons") => Value::list([]),
        // {solarSystemID: [instances] or count}; the Agency's anomaly trackers iterate these
        // as dicts and add the current system's sites from the sensor overlay themselves.
        ("dungeonInstanceCacheMgr", _) => Value::Dict(vec![]),
        // (pilots by solarSystemID, docked pilots by solarSystemID, divisor)
        ("machoNet", "GetClusterGameStatisticsForClient") => Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Int(1)]),
        // {warzoneID: {solarSystemID: occupationState}}: no system is contested yet, but every
        // warzone must be present (WarzoneStatusGauge indexes it by the faction's warzone).
        ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates") | ("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached") => {
            Value::Dict(WARZONES.iter().map(|(id, _)| (Value::Int(*id), Value::Dict(vec![]))).collect())
        }
        // {warzoneID: [factionID, ...]} (fwWarzoneSvc.GetWarzoneIdForFaction)
        ("fwWarzoneSolarsystem", "GetAllWarzones") => {
            Value::Dict(WARZONES.iter().map(|(id, factions)| (Value::Int(*id), Value::list(factions.iter().map(|f| Value::Int(*f))))).collect())
        }

        // ---- corporations ----
        ("corpRegistry", "GetCorporation") => {
            let corp = call.bind_params.and_then(|p| p.as_int().or_else(|| p.items().and_then(|t| t.first()?.as_int()))).unwrap_or(my_corp);
            return Some(corporation(c, corp).await);
        }
        ("corpmgr", "GetCorporations") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        // Show info on a corporation (corporationInfoWindow reads ceoID, stationID, shares...).
        ("corpmgr", "GetPublicInfo") => return Some(corporation(c, arg_int(0).unwrap_or(my_corp)).await),
        ("corpmgr", "GetAssetInventory") | ("corpmgr", "GetAssetInventoryForLocation") | ("corpmgr", "SearchAssets") => Value::list([]),
        // A player character's corporation, or an NPC's (agents) from the SDE.
        ("corpmgr", "GetCorporationIDForCharacter") => {
            let id = arg_int(0).unwrap_or(0);
            let r = c
                .db
                .client
                .query_opt(
                    "SELECT corporation_id FROM game.characters WHERE character_id = $1
                     UNION ALL SELECT (data->>'corporationID')::bigint FROM sde.npc_characters WHERE character_id = $1
                     LIMIT 1",
                    &[&id],
                )
                .await;
            match r {
                Ok(r) => r.and_then(|r| r.get::<_, Option<i64>>(0)).map(Value::Int).unwrap_or(Value::None),
                Err(e) => return Some(Err(db_err(e))),
            }
        }
        ("corpRegistry", "GetOldApplications") | ("corpRegistry", "GetAllianceApplications") => Value::Dict(vec![]),
        ("corpRegistry", "GetPendingAutoKicks") => Value::list([]),
        ("corpRegistry", "GetShareholders") => rowset(&["shareholderID", "shares", "shareholderCorporationID"], vec![]),
        ("corpRegistry", "GetSharesByShareholder") => rowset(&["shareholderID", "shares", "corporationID"], vec![]),
        ("corpRegistry", "GetStructureReinforceDefault") => Value::None,
        ("corpRegistry", "DoesMyCorpAcceptStructures") | ("corpRegistry", "DoesCorpRestrictCorpMails") => Value::Bool(false),
        ("warsInfoMgr", "GetWarsByOwnerID") => Value::Dict(vec![]),
        ("warsInfoMgr", "GetTop") | ("warsInfoMgr", "GetWarsRequiringAssistance") => Value::list([]),
        ("allianceRegistry", "GetDaysInAlliance") => Value::Int(0),
        ("allianceRegistry", "GetAllianceMembersOlderThan") => Value::list([]),

        // ---- wallet ----
        ("rewardMgr", "GetRewardLPLogs") => Value::list([]),
        ("account", "GetJournal") | ("account", "GetJournalForAccounts") | ("account", "GetTransactions") => Value::list([]),
        ("account", "GetWalletDivisionsInfo") => Value::list([]),
        ("lookupSvc", _) => Value::list([]),

        // ---- mail, notifications, contracts ----
        // labelID -> KeyVal(labelID, name, color); the static labels are added client-side
        ("mailingListsMgr", "GetJoinedLists") => Value::Dict(vec![]),
        // Client-side settings blobs (yaml); nothing reads them back yet.
        ("charMgr", "SaveCharacterSetting") => Value::None,

        ("map", "GetIncursionGlobalReport") => Value::list([]),
        // md5(str([])): the client's cached (empty) flagged-site list is current.
        ("browserLockdownSvc", "GetFlaggedSitesHash") => Value::str("d751713988987e9331980363e24189ce"),
        ("browserLockdownSvc", "GetFlaggedSitesList") => Value::list([]),
        ("charMgr", "GetPrivateInfoOnCorpChange") => Value::None,

        // ---- agency / job board ----
        ("dungeonExplorationMgr", "GetMyEscalatingPathDetails") => Value::list([]),
        // (factionID, directFactionID, corpFactionID): not enlisted.
        ("fwCharacterEnlistmentMgr", "GetMyEnlistment") => Value::tuple([Value::None, Value::None, Value::None]),
        // FILETIME the direct-enlistment cooldown ends; never enlisted, so none.
        ("fwCharacterEnlistmentMgr", "GetMyDirectEnlistmentCooldownTimestamp") => Value::Int(0),
        // Not enlisted in a militia: no ranks.
        ("facWarMgr", "GetMyCharacterRankOverview") => Value::list([]),
        // (medals, medalInfo): medals come from player corporations; medalInfo is only
        // Filter()ed per received medal.
        ("corporationSvc", "GetMedalsReceived") => Value::tuple([Value::list([]), Value::list([])]),
        // An NPC corporation's market supply and demand (typeID, sellPrice, buyPrice...): none traded.
        ("corporationSvc", "GetCorpInfo") => Value::list([]),
        // NPC corporations hold no shareholder votes.
        ("voteManager", "CanViewVotes") => Value::Bool(false),
        // No shared bookmark folders exist (accessGroupBookmarkMgr.AddFolder keeps none).
        ("accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess") => Value::list([]),

        // ---- probe / directional scanner (scanSvc.GetScanMan) ----
        ("scanMgr", "GetSystemScanMgr") => c.bind("scanMgr", Value::None),

        ("repairableShipMgr", "GetServiceLifeDamage") => Value::Float(0.0),
        ("monolithTaskTrackerMgr", "UpdateClientAchievmentsAndCounters") => Value::None,
        // typeID -> expert system state; none installed.
        ("expertSystemMgr", "GetMyExpertSystems") => Value::Dict(vec![]),
        // The adaptation tree answers through notifications; there are no trees to send.
        ("shipAdaptionMgr", "fetch_tree_data") | ("shipAdaptionMgr", "fetch_missions") => Value::None,
        _ => {
            let _ = me;
            return None;
        }
    }))
}
