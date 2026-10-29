//! New Eden Store, PLEX vault, redeeming queue and seasonal login rewards.
//!
//! * `storeManager` (vgs/client/vgsClient.py): the catalogue as the JSON-shaped
//!   dicts vgs/common/utils.py parses. The live catalogue was server-side only, so
//!   it is rebuilt from this build's data: Serenity-visible SKINs, apparel and
//!   crates that traded on the retail market, priced in PLEX at the retail
//!   ISK/PLEX ratio (crate::retail), plus skill services and a free daily gift.
//!   Category ids are the ones the store UI has icons for (nes_constants.py).
//! * `vaultManager`: the account's PLEX.
//! * `userSvc` redeem tokens (eve/client/script/ui/services/redeemsvc.py): what
//!   purchases and rewards deliver; claiming turns them into items, or free skill
//!   points for the skill-point types the client lists in redeem/data.py.
//! * `seasonalLoginCampaignManager` (loginrewards/client/seasonalLoginCampaignService.py):
//!   the newest seasonal campaign in seasonalCampaignsLoginRewards, one claim per day.

use super::dogma::load_items;
use super::{keyval, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::services::charsel::FLAG_HANGAR;
use marshal::Value;
use std::sync::OnceLock;
use tracing::info;

fn db_err(e: tokio_postgres::Error) -> CallError {
    CallError::Internal(e.into())
}

const PLX: &str = "PLX";
const TYPE_PLEX: i32 = 44992;
const PERIOD_DAY: i64 = 1;
/// The daily free gift: 10,000 skill points.
const DAILY_GIFT_OFFER: i64 = 1;
const DAILY_GIFT_TYPE: i32 = 52520;
/// Daily turnover of login rewards and daily offers (UTC; 11:00 Beijing time).
const TURNOVER: &str = "03:00:00";
const REWARD_DAY_SQL: &str = "(now() AT TIME ZONE 'UTC' - interval '3 hours')::date";

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

// ---------------------------------------------------------------- catalogue

struct Category {
    id: i64,
    name: String,
    parent: Option<i64>,
    home: bool,
}

struct Offer {
    id: i64,
    type_id: i32,
    quantity: i64,
    name: String,
    image: String,
    category: i64,
    price: i64,
    /// Free once per day (`isPeriodReward`, claimed through `free_to_get`).
    daily: bool,
}

#[derive(Default)]
struct Catalogue {
    categories: Vec<Category>,
    offers: Vec<Offer>,
}

/// Localized (zh) name and icon of a type, joined onto `t`.
const TYPE_INFO_JOINS: &str = "
    LEFT JOIN sde_native.types nt ON nt.key = t.type_id::text
    LEFT JOIN sde_native.localization_fsd_zh ln ON ln.key = nt.data->>'typeNameID'
    LEFT JOIN sde_native.iconids ic ON ic.key = nt.data->>'iconID'";
const TYPE_INFO_COLUMNS: &str = "t.type_id, COALESCE(ln.data->>'text', t.name), COALESCE(ic.data->>'iconFile', '')";

async fn build_catalogue(c: &Client) -> Result<Catalogue, CallError> {
    let db = &c.db.client;
    let plex_isk: f64 = db
        .query_opt("SELECT average FROM game.retail_prices WHERE type_id = $1", &[&TYPE_PLEX])
        .await
        .map_err(db_err)?
        .map(|r| r.get(0))
        .unwrap_or(23_500_000.0);
    let to_plex = |isk: f64| ((isk / plex_isk).round() as i64).max(1);
    let mut cat = Catalogue::default();
    let mut root = |id: i64, name: &str, home: bool| cat.categories.push(Category { id, name: name.into(), parent: None, home });
    root(62, "每日礼包", true);
    root(1, "舰船涂装", false);
    root(2, "技能与服务", false);
    root(21, "服饰", false);
    root(49, "补给箱", false);
    let sub = |cat: &mut Catalogue, id: i64, name: &str, parent: i64| {
        cat.categories.push(Category { id, name: name.into(), parent: Some(parent), home: false })
    };
    sub(&mut cat, 6201, "每日免费", 62);
    sub(&mut cat, 201, "技能注入", 2);
    sub(&mut cat, 4901, "补给箱", 49);
    for (id, name) in [(101, "艾玛"), (102, "加达里"), (103, "盖伦特"), (104, "米玛塔尔"), (105, "其他")] {
        sub(&mut cat, id, name, 1);
    }

    // Daily free gift.
    let row = db
        .query_one(&format!("SELECT {TYPE_INFO_COLUMNS} FROM sde.inv_types t {TYPE_INFO_JOINS} WHERE t.type_id = $1"), &[&DAILY_GIFT_TYPE])
        .await
        .map_err(db_err)?;
    cat.offers.push(Offer {
        id: DAILY_GIFT_OFFER,
        type_id: DAILY_GIFT_TYPE,
        quantity: 1,
        name: row.get(1),
        image: row.get(2),
        category: 6201,
        price: 0,
        daily: true,
    });

    // Skill services: retail price where it traded, else the store's list price.
    for (type_id, list_price) in [(40520, 250), (45635, 50), (34133, 485)] {
        let row = db
            .query_one(
                &format!("SELECT {TYPE_INFO_COLUMNS}, r.average FROM sde.inv_types t {TYPE_INFO_JOINS}
                          LEFT JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0 WHERE t.type_id = $1"),
                &[&type_id],
            )
            .await
            .map_err(db_err)?;
        let price = row.get::<_, Option<f64>>(3).map(to_plex).unwrap_or(list_price);
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: row.get(1), image: row.get(2), category: 201, price, daily: false });
    }

    // Permanent SKINs visible on Serenity that traded there, by the hull's race.
    let rows = db
        .query(
            &format!(
                "SELECT {TYPE_INFO_COLUMNS}, r.average, COALESCE(hull.race_id, 0)
                 FROM sde_native.skinlicenses l
                 JOIN sde_native.skins s ON s.key = l.data->>'skinID'
                 JOIN sde.inv_types t ON t.type_id = l.key::int
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 LEFT JOIN sde.inv_types hull ON hull.type_id = (s.data->'types'->>0)::int
                 {TYPE_INFO_JOINS}
                 WHERE t.published AND (s.data->>'visibleSerenity')::boolean AND (l.data->>'duration')::int = -1
                 ORDER BY t.type_id"
            ),
            &[],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let category = match r.get::<_, i32>(4) {
            4 => 101,
            1 => 102,
            8 => 103,
            2 => 104,
            _ => 105,
        };
        let type_id: i32 = r.get(0);
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.get(1), image: r.get(2), category, price: to_plex(r.get(3)), daily: false });
    }

    // Apparel that traded on Serenity, under its market group's parent (男装, 女装, 饰品...).
    let rows = db
        .query(
            &format!(
                "SELECT {TYPE_INFO_COLUMNS}, r.average, sub.key::bigint, COALESCE(sn.data->>'text', '服饰')
                 FROM sde.inv_types t
                 JOIN sde.inv_categories c ON c.category_id = t.category_id AND c.name = 'Apparel'
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 JOIN sde_native.marketgroups mg ON mg.key = t.market_group_id::text
                 JOIN sde_native.marketgroups sub ON sub.key = COALESCE(mg.data->>'parentGroupID', mg.key)
                 LEFT JOIN sde_native.localization_fsd_zh sn ON sn.key = sub.data->>'nameID'
                 {TYPE_INFO_JOINS}
                 WHERE t.published
                 ORDER BY t.type_id"
            ),
            &[],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let category = 21_000_000 + r.get::<_, i64>(4);
        if !cat.categories.iter().any(|c| c.id == category) {
            sub(&mut cat, category, &r.get::<_, String>(5), 21);
        }
        let type_id: i32 = r.get(0);
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.get(1), image: r.get(2), category, price: to_plex(r.get(3)), daily: false });
    }

    // Crates that traded on Serenity, with their splash art.
    let rows = db
        .query(
            &format!(
                "SELECT {TYPE_INFO_COLUMNS}, r.average, COALESCE(cr.data->'staticSplash'->>'resPath', '')
                 FROM sde_native.crates cr
                 JOIN sde.inv_types t ON t.type_id = (cr.data->>'typeID')::int
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 {TYPE_INFO_JOINS}
                 WHERE t.published
                 ORDER BY t.type_id"
            ),
            &[],
        )
        .await
        .map_err(db_err)?;
    for r in rows {
        let type_id: i32 = r.get(0);
        let splash: String = r.get(4);
        let image = if splash.is_empty() { r.get(2) } else { splash };
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.get(1), image, category: 4901, price: to_plex(r.get(3)), daily: false });
    }
    info!("store: {} offers in {} categories", cat.offers.len(), cat.categories.len());
    Ok(cat)
}

async fn catalogue(c: &Client) -> Result<&'static Catalogue, CallError> {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    if let Some(cat) = CATALOGUE.get() {
        return Ok(cat);
    }
    let built = build_catalogue(c).await?;
    Ok(CATALOGUE.get_or_init(|| built))
}

fn s(v: &str) -> Value {
    Value::unicode(v)
}

fn category_json(cat: &Category) -> Value {
    Value::dict([
        ("id", Value::Int(cat.id)),
        ("name", s(&cat.name)),
        ("href", s("")),
        ("parent", cat.parent.map(|p| Value::dict([("id", Value::Int(p))])).unwrap_or(Value::None)),
        ("tags", Value::list([])),
        ("isHomePage", Value::Bool(cat.home)),
        ("sortPriority", Value::Int(cat.id)),
    ])
}

fn product_json(o: &Offer) -> Value {
    Value::dict([
        ("id", Value::Int(o.type_id as i64)),
        ("name", s(&o.name)),
        ("href", s("")),
        ("typeId", Value::Int(o.type_id as i64)),
        ("quantity", Value::Int(o.quantity)),
        ("productName", s(&o.name)),
        ("imageUrl", s(&o.image)),
    ])
}

/// `YYYY-MM-DDTHH:MM:SSZ` of the current daily period's start.
fn period_start() -> String {
    let secs = (filetime_now() - 116_444_736_000_000_000) / 10_000_000 - 3 * 3600;
    let day = secs.div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{TURNOVER}Z")
}

fn offer_json(o: &Offer, can_purchase: bool) -> Value {
    let mut fields = vec![
        ("id", Value::Int(o.id)),
        ("name", s(&o.name)),
        ("description", s("")),
        ("href", s("")),
        ("offerPricings", Value::list([Value::dict([("currency", Value::str(PLX)), ("price", Value::Int(o.price)), ("basePrice", Value::Int(o.price))])])),
        ("imageUrl", s(&o.image)),
        ("products", Value::list([product_json(o)])),
        ("categories", Value::list([Value::dict([("id", Value::Int(o.category))])])),
        ("label", Value::None),
        ("canPurchase", Value::Bool(can_purchase)),
        ("singlePurchase", Value::Bool(false)),
        ("sortPriority", Value::Int(o.id)),
    ];
    if o.daily {
        fields.extend([("periodPurchase", Value::Int(PERIOD_DAY)), ("isPeriodReward", Value::Bool(true)), ("periodStartTime", s(&period_start()))]);
    }
    Value::dict(fields)
}

async fn claimed_daily_gift(c: &Client) -> Result<bool, CallError> {
    Ok(c.db
        .client
        .query_one(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM game.store_purchases WHERE user_id = $1 AND offer_id = $2
                 AND (purchased_at AT TIME ZONE 'UTC' - interval '3 hours')::date = {REWARD_DAY_SQL})"
            ),
            &[&c.login.user_id, &(DAILY_GIFT_OFFER as i32)],
        )
        .await
        .map_err(db_err)?
        .get(0))
}

// ---------------------------------------------------------------- PLEX and redeeming

async fn plex_balance(c: &Client) -> Result<i64, CallError> {
    Ok(c.db
        .client
        .query_one("SELECT plex_balance FROM game.accounts WHERE user_id = $1", &[&c.login.user_id])
        .await
        .map_err(db_err)?
        .get(0))
}

/// Queue `quantity` x `type_id` on `user_id`'s redeeming queue.
async fn grant_token(c: &Client, user_id: i64, type_id: i32, quantity: i64, label: &str) -> Result<(), CallError> {
    c.db.client
        .execute(
            "INSERT INTO game.redeem_tokens (user_id, type_id, quantity, label, created) VALUES ($1, $2, $3, $4, $5)",
            &[&user_id, &type_id, &(quantity as i32), &label, &filetime_now()],
        )
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn redeem_tokens(c: &Client) -> CallResult {
    let rows = c
        .db
        .client
        .query(
            "SELECT token_id, type_id, quantity, label, description, created FROM game.redeem_tokens WHERE user_id = $1 ORDER BY token_id",
            &[&c.login.user_id],
        )
        .await
        .map_err(db_err)?;
    let tokens = rows.iter().map(|r| {
        keyval([
            ("tokenID", Value::Int(r.get(0))),
            ("massTokenID", Value::None),
            ("typeID", Value::Int(r.get::<_, i32>(1) as i64)),
            ("quantity", Value::Int(r.get::<_, i32>(2) as i64)),
            ("blueprintRuns", Value::None),
            ("blueprintMaterialLevel", Value::None),
            ("blueprintProductivityLevel", Value::None),
            ("stationID", Value::None),
            ("expireDateTime", Value::None),
            ("dateTime", Value::Int(r.get(5))),
            ("label", s(&r.get::<_, String>(3))),
            ("description", s(&r.get::<_, String>(4))),
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
    let row = c
        .db
        .client
        .query_opt("SELECT home_station_id, station_id FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&char_id, &c.login.user_id])
        .await
        .map_err(db_err)?
        .ok_or_else(|| CallError::User { msg: "CharacterNotFound".into(), dict: vec![] })?;
    let docked = if c.session.int("charid") == Some(char_id) { c.session.int("stationid") } else { row.get(1) };
    let station: Option<i64> = if home { row.get(0) } else { docked.or(row.get(0)) };
    let tokens = c
        .db
        .client
        .query("DELETE FROM game.redeem_tokens WHERE user_id = $1 AND token_id = ANY($2) RETURNING type_id, quantity", &[&c.login.user_id, &ids])
        .await
        .map_err(db_err)?;
    let mut created = vec![];
    let mut skill_points = 0i64;
    for t in &tokens {
        let (type_id, quantity): (i32, i32) = (t.get(0), t.get(1));
        if let Some((_, sp)) = SKILL_POINT_TYPES.iter().find(|(id, _)| *id == type_id) {
            skill_points += sp * quantity as i64;
            continue;
        }
        let Some(station) = station else { continue };
        let item_id: i64 = c
            .db
            .client
            .query_one(
                "INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, $2, $3, $4, $5) RETURNING item_id",
                &[&type_id, &char_id, &station, &FLAG_HANGAR, &quantity],
            )
            .await
            .map_err(db_err)?
            .get(0);
        created.push((item_id, type_id, quantity));
    }
    if skill_points > 0 {
        let free: i64 = c
            .db
            .client
            .query_one(
                "UPDATE game.characters SET free_skill_points = free_skill_points + $2 WHERE character_id = $1 RETURNING free_skill_points",
                &[&char_id, &skill_points],
            )
            .await
            .map_err(db_err)?
            .get(0);
        if c.session.int("charid") == Some(char_id) {
            c.notify("OnFreeSkillPointsChanged", vec![Value::Int(free)]);
        }
    }
    if station.is_some() && station == c.session.int("stationid") && c.session.int("charid") == Some(char_id) {
        for (item_id, ..) in &created {
            if let Some(item) = load_items(c, "i.item_id = $1", &[item_id]).await?.into_iter().next() {
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

// ---------------------------------------------------------------- store calls

/// buy_offer(offer_id, currency, qty, from_character_id=, to_character_id=, ...) -> success
async fn buy_offer(c: &mut Client, call: &Call<'_>) -> CallResult {
    let offer_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let qty = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1).max(1);
    let gift_to = call.kwargs.get("to_character_id").and_then(|v| v.as_int());
    let cat = catalogue(c).await?;
    let Some(offer) = cat.offers.iter().find(|o| o.id == offer_id && !o.daily) else { return Ok(Value::Bool(false)) };
    let cost = offer.price * qty;
    let recipient = match gift_to {
        Some(ch) => match c.db.client.query_opt("SELECT user_id FROM game.characters WHERE character_id = $1", &[&ch]).await.map_err(db_err)? {
            Some(r) => r.get(0),
            None => return Ok(Value::Bool(false)),
        },
        None => c.login.user_id,
    };
    let Some(row) = c
        .db
        .client
        .query_opt(
            "UPDATE game.accounts SET plex_balance = plex_balance - $2 WHERE user_id = $1 AND plex_balance >= $2 RETURNING plex_balance",
            &[&c.login.user_id, &cost],
        )
        .await
        .map_err(db_err)?
    else {
        info!(user = c.login.user_id, "store: cannot afford offer {offer_id} x{qty} ({cost} PLEX)");
        return Ok(Value::Bool(false));
    };
    let balance: i64 = row.get(0);
    c.db.client
        .execute(
            "INSERT INTO game.store_purchases (user_id, offer_id, quantity, price) VALUES ($1, $2, $3, $4)",
            &[&c.login.user_id, &(offer_id as i32), &(qty as i32), &cost],
        )
        .await
        .map_err(db_err)?;
    grant_token(c, recipient, offer.type_id, offer.quantity * qty, "UI/VirtualGoodsStore/StoreName").await?;
    info!(user = c.login.user_id, "store: bought offer {offer_id} ({}) x{qty} for {cost} PLEX", offer.name);
    c.notify("OnAurumChangeFromVgs", vec![Value::dict([("balance", Value::Int(balance))])]);
    if recipient == c.login.user_id {
        c.notify("OnRedeemingQueueUpdated", vec![]);
    }
    Ok(Value::Bool(true))
}

/// free_to_get(offer_id, is_game_time=, store_id=): the daily gift.
async fn free_to_get(c: &mut Client, call: &Call<'_>) -> CallResult {
    let offer_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let cat = catalogue(c).await?;
    let Some(offer) = cat.offers.iter().find(|o| o.id == offer_id && o.daily) else { return Ok(Value::Bool(false)) };
    if claimed_daily_gift(c).await? {
        return Ok(Value::Bool(false));
    }
    c.db.client
        .execute(
            "INSERT INTO game.store_purchases (user_id, offer_id, quantity, price) VALUES ($1, $2, 1, 0)",
            &[&c.login.user_id, &(offer_id as i32)],
        )
        .await
        .map_err(db_err)?;
    grant_token(c, c.login.user_id, offer.type_id, offer.quantity, "UI/VirtualGoodsStore/StoreName").await?;
    c.notify("OnRedeemingQueueUpdated", vec![]);
    Ok(Value::Bool(true))
}

// ---------------------------------------------------------------- seasonal login rewards

/// The newest seasonal campaign: (id, alpha rewards, omega rewards), each reward `(typeID, quantity)` by day.
async fn seasonal_campaign(c: &Client) -> Result<Option<(i32, Vec<(i32, i64)>, Vec<(i32, i64)>)>, CallError> {
    // Rewards as `typeID:quantity,...` in day order (jsonb {"0": {"typeID", "quantity"}, ...}).
    let track = |col: &str| {
        format!(
            "(SELECT string_agg((v->>'typeID') || ':' || COALESCE(v->>'quantity', '1'), ',' ORDER BY k::int)
              FROM jsonb_each(data->'{col}') AS e(k, v))"
        )
    };
    let row = c
        .db
        .client
        .query_opt(
            &format!(
                "SELECT key::int, {}, {} FROM sde_native.seasonalcampaignsloginrewards
                 WHERE data ? 'alphaRewards' AND data ? 'omegaRewards' ORDER BY key::int DESC LIMIT 1",
                track("alphaRewards"),
                track("omegaRewards")
            ),
            &[],
        )
        .await
        .map_err(db_err)?;
    let Some(row) = row else { return Ok(None) };
    let rewards = |i: usize| parse_rewards(&row.get::<_, Option<String>>(i).unwrap_or_default());
    Ok(Some((row.get(0), rewards(1), rewards(2))))
}

/// `typeID:quantity,typeID:quantity,...`
fn parse_rewards(s: &str) -> Vec<(i32, i64)> {
    s.split(',').filter_map(|p| {
        let (t, q) = p.split_once(':')?;
        Some((t.parse().ok()?, q.parse().ok()?))
    })
    .collect()
}

/// (claims so far, claimed today) for `campaign`.
async fn seasonal_claims(c: &Client, campaign: i32) -> Result<(i64, bool), CallError> {
    let r = c
        .db
        .client
        .query_one(
            &format!("SELECT count(*), count(*) FILTER (WHERE day = {REWARD_DAY_SQL}) > 0 FROM game.login_reward_claims WHERE user_id = $1 AND campaign_id = $2"),
            &[&c.login.user_id, &campaign],
        )
        .await
        .map_err(db_err)?;
    Ok((r.get(0), r.get(1)))
}

async fn seasonal(c: &mut Client, call: &Call<'_>) -> CallResult {
    let Some((campaign, alpha, omega)) = seasonal_campaign(c).await? else {
        return Ok(match call.method {
            "get_active_campaign" => Value::tuple([Value::None, Value::None, Value::None, Value::None]),
            "get_claim_history" => Value::Dict(vec![]),
            _ => Value::Bool(false),
        });
    };
    let total = alpha.len().min(omega.len()) as i64;
    let (claimed, today) = seasonal_claims(c, campaign).await?;
    match call.method {
        // (campaignID, end date ordinal, daily turnover, rewards remaining). The campaign stays
        // open until every reward is claimed: it ends the day after the last one would be.
        "get_active_campaign" => {
            let remaining = total - claimed;
            let today_ordinal: i32 =
                c.db.client.query_one(&format!("SELECT ({REWARD_DAY_SQL} - date '0001-01-01') + 1"), &[]).await.map_err(db_err)?.get(0);
            let end = today_ordinal as i64 + remaining + 1;
            Ok(Value::tuple([Value::Int(campaign as i64), Value::Int(end), Value::str(TURNOVER), Value::Int(remaining)]))
        }
        // {date ordinal: clone grade}
        "get_claim_history" => {
            let rows = c
                .db
                .client
                .query(
                    "SELECT (day - date '0001-01-01') + 1, clone_grade FROM game.login_reward_claims WHERE user_id = $1 AND campaign_id = $2",
                    &[&c.login.user_id, &campaign],
                )
                .await
                .map_err(db_err)?;
            Ok(Value::Dict(rows.iter().map(|r| (Value::Int(r.get::<_, i32>(0) as i64), Value::Int(r.get::<_, i32>(1) as i64))).collect()))
        }
        // Omega clones get both tracks' reward for the day.
        "claim_reward" => {
            if today || claimed >= total {
                return Ok(Value::Bool(false));
            }
            c.db.client
                .execute(
                    &format!("INSERT INTO game.login_reward_claims (user_id, campaign_id, day, clone_grade) VALUES ($1, $2, {REWARD_DAY_SQL}, 1)"),
                    &[&c.login.user_id, &campaign],
                )
                .await
                .map_err(db_err)?;
            let day = claimed as usize;
            for (type_id, quantity) in [alpha[day], omega[day]] {
                grant_token(c, c.login.user_id, type_id, quantity, "UI/LoginRewards/WindowCaption").await?;
            }
            info!(user = c.login.user_id, "login reward: campaign {campaign} day {} claimed", day + 1);
            c.notify("OnRedeemingQueueUpdated", vec![]);
            Ok(Value::Bool(true))
        }
        _ => Ok(Value::None),
    }
}

// ---------------------------------------------------------------- dispatch

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    Some(match (call.service, call.method) {
        ("storeManager", "get_categories") => catalogue(c).await.map(|cat| Value::list(cat.categories.iter().map(category_json))),
        ("storeManager", "get_products") => catalogue(c).await.map(|cat| Value::list(cat.offers.iter().map(product_json))),
        ("storeManager", "get_offers") => match (catalogue(c).await, claimed_daily_gift(c).await) {
            (Ok(cat), Ok(claimed)) => Ok(Value::list(cat.offers.iter().map(|o| offer_json(o, !(o.daily && claimed))))),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        ("storeManager", "get_offer_states") => match (catalogue(c).await, claimed_daily_gift(c).await) {
            (Ok(cat), Ok(claimed)) => Ok(Value::list(cat.offers.iter().map(|o| {
                let mut state = vec![("offerId", Value::Int(o.id)), ("canPurchase", Value::Bool(!(o.daily && claimed)))];
                if o.daily {
                    state.extend([("periodPurchase", Value::Int(PERIOD_DAY)), ("isPeriodReward", Value::Bool(true))]);
                }
                Value::dict(state)
            }))),
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
        ("storeManager", "get_offer_groups") => Ok(Value::list([])),
        ("storeManager", "buy_offer") => buy_offer(c, call).await,
        ("storeManager", "free_to_get") => free_to_get(c, call).await,
        ("storeManager", "can_purchase_offer") | ("storeManager", "is_offer_purchasable") => Ok(Value::Bool(true)),
        ("storeManager", "set_offer_purchased") => Ok(Value::None),

        // get_account_balance(currency): PLEX in the vault; no other currencies.
        ("vaultManager", "get_account_balance") => {
            if call.args.first().and_then(|v| v.as_text()).as_deref() == Some(PLX) {
                plex_balance(c).await.map(Value::Int)
            } else {
                Ok(Value::Int(0))
            }
        }

        ("userSvc", "GetRedeemTokens") => redeem_tokens(c).await,
        ("userSvc", "ClaimRedeemTokens") => claim_tokens(c, call).await,
        ("userSvc", "TrashRedeemTokens") => {
            let ids = token_ids(call.args.first());
            match c.db.client.execute("DELETE FROM game.redeem_tokens WHERE user_id = $1 AND token_id = ANY($2)", &[&c.login.user_id, &ids]).await {
                Ok(_) => Ok(Value::None),
                Err(e) => Err(db_err(e)),
            }
        }

        // Fast checkout is the real-money PLEX purchase flow (fastcheckout/client): off. The China
        // funnel sends "buy PLEX" to the store instead of a card checkout.
        // GetTestingConfiguration -> (isChinaFunnel, fakeBuyPlexOfferUrl, useShellExecute)
        ("FastCheckoutService", "GetTestingConfiguration") => Ok(Value::tuple([Value::Bool(true), s(""), Value::Bool(false)])),
        ("FastCheckoutService", "IsFastCheckoutEnabledForUser") => Ok(Value::Bool(false)),
        ("FastCheckoutService", "ClearFastCheckoutCacheForUser") => Ok(Value::None),
        ("FastCheckoutService", "GetOffersForUser") => Ok(Value::dict([("plex", Value::list([]))])),
        ("FastCheckoutService", "BuyOffer") => Ok(Value::Bool(false)),

        ("seasonalLoginCampaignManager", _) => seasonal(c, call).await,
        ("loginRewardFacilities", "get_my_clone_grade") => Ok(Value::Int(1)), // CLONE_STATE_OMEGA
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_start_is_a_utc_timestamp() {
        let p = period_start();
        assert_eq!(p.len(), 20);
        assert!(p.ends_with("T03:00:00Z"), "{p}");
    }

    #[test]
    fn rewards_parse() {
        assert_eq!(parse_rewards("57003:1,72409:2"), vec![(57003, 1), (72409, 2)]);
        assert!(parse_rewards("").is_empty());
    }
}
