//! userSvc redeem queue for eve/client/script/ui/services/redeemsvc.py.
//! Uses game redeem tokens, characters and items; unsupported token types follow existing delivery policy.
use super::*;
/// Skill points granted per redeemed type (redeem/data.py `get_redeem_data`).
const SKILL_POINT_TYPES: &[(i32, i64)] = &[
    (60550, 100), (60551, 500), (54652, 1000), (54653, 2500), (63303, 4000), (63304, 4500), (54650, 5000), (63305, 5500),
    (63306, 6000), (63307, 6500), (63308, 7000), (54654, 7500), (63309, 8000), (63310, 9000), (52520, 10000), (63311, 11000),
    (63312, 12000), (63313, 13000), (63314, 14000), (63315, 15000), (63316, 16000), (63317, 17000), (63318, 18000),
    (63623, 19000), (63624, 20000), (63319, 21000), (63320, 22000), (63321, 23000), (63322, 24000), (49756, 25000),
    (63323, 27000), (63324, 28000), (63325, 29000), (57003, 30000), (63326, 31000), (63327, 32000), (63328, 33000),
    (63329, 34000), (63330, 35000), (63331, 42000), (63332, 43000), (63333, 44000), (63334, 45000), (63335, 46000),
    (63336, 47000), (63337, 48000), (63338, 49000), (49809, 50000), (63339, 51000), (63340, 52000), (63824, 75000),
    (49810, 100000), (63825, 125000), (54648, 150000), (43680, 250000), (52269, 500000), (89839, 648000), (52270, 750000),
    (52318, 750000), (52263, 1000000), (56707, 1620000),
];
async fn redeem_tokens(c: &Client) -> CallResult {
    let rows = crate::dao::store::purchases::tokens(&c.db, c.login.user_id).await.map_err(db_err)?;
    let tokens = rows.iter().map(|r| {
        keyval([
            ("tokenID", Value::Int(r.token_id)),
            ("massTokenID", Value::None),
            ("typeID", Value::Int(r.type_id as i64)),
            ("quantity", Value::Int(r.quantity as i64)),
            ("blueprintRuns", Value::None),
            ("blueprintMaterialLevel", Value::None),
            ("blueprintProductivityLevel", Value::None),
            ("stationID", Value::None),
            ("expireDateTime", Value::None),
            ("dateTime", Value::Int(r.created)),
            ("label", s(&r.label)),
            ("description", s(&r.description)),
            ("soulbound", Value::Bool(false)),
        ])
    });
    // (tokens, soulbound (tokenID, massTokenID) pairs)
    Ok(Value::tuple([Value::list(tokens), Value::list([])]))
}

/// `tokenID`s of the `[{'tokenID', 'massTokenID', 'typeID'}]` the client sends.
fn token_ids(v: Option<&Value>) -> Vec<i64> {
    v.and_then(|v| v.items())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| match t {
            Value::Instance(_, state) => state.get("tokenID"),
            other => other.get("tokenID"),
        })
        .filter_map(|v| v.as_int())
        .collect()
}

/// ClaimRedeemTokens(tokens, charID, useHomeStation=False) -> {'errors', 'itemsCreated', 'stationID',
/// 'isk_balance', 'activated_licenses', 'isk_rewarded'}
async fn claim_tokens(c: &mut Client, call: &Call<'_>) -> CallResult {
    let ids = token_ids(call.args.first());
    let char_id = call.args.get(1).and_then(|v| v.as_int()).or_else(|| c.session.int("charid")).unwrap_or(0);
    let home = call.kwargs.get("useHomeStation").or_else(|| call.args.get(2)).and_then(|v| v.as_int()).unwrap_or(0) != 0;
    let row = crate::dao::store::purchases::stations(&c.db, char_id, c.login.user_id).await.map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: messages::MSG_CHARACTER_NOT_FOUND.into(), dict: vec![] })?;
    let docked = if c.session.int("charid") == Some(char_id) { c.session.int("stationid") } else { row.station_id };
    let station: Option<i64> = if home { row.home_station_id } else { docked.or(row.home_station_id) };
    let tokens = crate::dao::store::purchases::claim(&c.db, c.login.user_id, &ids).await.map_err(db_err)?;
    let mut created = vec![];
    let mut skill_points = 0i64;
    for t in &tokens {
        let (type_id, quantity) = (t.type_id, t.quantity);
        if let Some((_, sp)) = SKILL_POINT_TYPES.iter().find(|(id, _)| *id == type_id) {
            skill_points += sp * quantity as i64;
            continue;
        }
        let Some(station) = station else { continue };
        let item_id = crate::dao::items::transfers::insert(&c.db, type_id, char_id, station, FLAG_HANGAR, quantity).await.map_err(db_err)?;
        created.push((item_id, type_id, quantity));
    }
    if skill_points > 0 {
        let free = crate::dao::store::purchases::add_points(&c.db, char_id, skill_points).await.map_err(db_err)?;
        if c.session.int("charid") == Some(char_id) {
            c.notify("OnFreeSkillPointsChanged", vec![Value::Int(free)]);
        }
    }
    if station.is_some() && station == c.session.int("stationid") && c.session.int("charid") == Some(char_id) {
        for (item_id, ..) in &created {
            if let Some(item) = load_items(&c.db, &c.sde, ItemQuery::Id(*item_id)).await.map_err(db_err)?.into_iter().next() {
                c.notify("OnItemChange", vec![item.row(), Value::Dict(vec![(Value::Int(3), Value::Int(0))]), Value::None]);
            }
        }
    }
    info!(user = c.login.user_id, "redeemed {} tokens onto {char_id}: {} items, {skill_points} SP", tokens.len(), created.len());
    let items = created.iter().map(|(item, t, q)| {
        Value::dict([("itemID", Value::Int(*item)), ("typeID", Value::Int(*t as i64)), ("quantity", Value::Int(*q as i64))])
    });
    Ok(Value::dict([
        ("errors", Value::list([])),
        ("itemsCreated", Value::list(items)),
        ("stationID", station.map(Value::Int).unwrap_or(Value::None)),
        ("isk_balance", Value::None),
        // RedeemItemsContainer._OnRedeem indexes these when no items were created (SP tokens).
        ("activated_licenses", Value::list([])),
        ("isk_rewarded", Value::Int(0)),
    ]))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("userSvc", "GetRedeemTokens") => redeem_tokens(c).await,
        ("userSvc", "ClaimRedeemTokens") => claim_tokens(c, call).await,
        ("userSvc", "TrashRedeemTokens") => {
            let ids = token_ids(call.args.first());
            match crate::dao::store::purchases::trash(&c.db, c.login.user_id, &ids).await {
                Ok(_) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }

        _ => return None,
    })
}
