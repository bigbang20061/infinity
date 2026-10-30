//! storeManager catalogue and purchases for vgs/client/vgsClient.py.
//! Uses SDE types and retail prices, `game.accounts`, `game.store_purchases` and
//! `game.redeem_tokens`; live catalogue data is unavailable. Account PLEX is exact
//! up to 36 digits in storage, with the client-facing i64 projection clamped by the
//! DAO. Balance notifications include `userid`; clients cannot display oversized
//! balances exactly.
use super::*;
use crate::dao::store::catalogue::{TYPE_INFO_COLUMNS, TYPE_INFO_JOINS};
const PLX: &str = "PLX";
const TYPE_PLEX: i32 = 44992;
const PERIOD_DAY: i64 = 1;
/// The daily free gift: 10,000 skill points.
const DAILY_GIFT_OFFER: i64 = 1;
const DAILY_GIFT_TYPE: i32 = 52520;
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

async fn build_catalogue(c: &Client) -> Result<Catalogue, CallError> {
    let db = &c.db;
    let plex_isk = crate::dao::store::catalogue::plex_price(db, TYPE_PLEX).await.map_err(db_err)?.unwrap_or(23_500_000.0);
    let to_plex = |isk: f64| ((isk / plex_isk).round() as i64).max(1);
    let mut cat = Catalogue::default();
    let mut root = |id: i64, name: &str, home: bool| cat.categories.push(Category { id, name: name.into(), parent: None, home });
    root(62, messages::DAILY_GIFT_CATEGORY_NAME, true);
    root(1, messages::SHIP_SKIN_CATEGORY_NAME, false);
    root(2, messages::SKILLS_AND_SERVICES_CATEGORY_NAME, false);
    root(21, messages::APPAREL_CATEGORY_NAME, false);
    root(49, messages::SUPPLY_CRATE_CATEGORY_NAME, false);
    let sub = |cat: &mut Catalogue, id: i64, name: &str, parent: i64| {
        cat.categories.push(Category { id, name: name.into(), parent: Some(parent), home: false })
    };
    sub(&mut cat, 6201, messages::DAILY_FREE_CATEGORY_NAME, 62);
    sub(&mut cat, 201, messages::SKILL_INJECTOR_CATEGORY_NAME, 2);
    sub(&mut cat, 4901, messages::SUPPLY_CRATE_CATEGORY_NAME, 49);
    for (id, name) in [(101, messages::AMARR_CATEGORY_NAME), (102, messages::CALDARI_CATEGORY_NAME), (103, messages::GALLENTE_CATEGORY_NAME), (104, messages::MINMATAR_CATEGORY_NAME), (105, messages::OTHER_FACTION_CATEGORY_NAME)] {
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
        name: row.get("name"),
        image: row.get("image"),
        category: 6201,
        price: 0,
        daily: true,
    });

    // Skill services: retail price where it traded, else the store's list price.
    for (type_id, list_price) in [(40520, 250), (45635, 50), (34133, 485)] {
        let row = crate::dao::store::catalogue::service(db, type_id).await.map_err(db_err)?;
        let price = row.average.map(to_plex).unwrap_or(list_price);
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: row.name, image: row.image, category: 201, price, daily: false });
    }

    // Permanent SKINs visible on Serenity that traded there, by the hull's race.
    let rows = crate::dao::store::catalogue::skins(db).await.map_err(db_err)?;
    for r in rows {
        let category = match r.race_id {
            4 => 101,
            1 => 102,
            8 => 103,
            2 => 104,
            _ => 105,
        };
        let type_id = r.type_id;
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.name, image: r.image, category, price: to_plex(r.average), daily: false });
    }

    // Apparel that traded on Serenity, under its market group's parent (男装, 女装, 饰品...).
    let rows = crate::dao::store::catalogue::apparel(db, messages::APPAREL_CATEGORY_NAME).await.map_err(db_err)?;
    for r in rows {
        let category = 21_000_000 + r.group_id;
        if !cat.categories.iter().any(|c| c.id == category) {
            sub(&mut cat, category, &r.group_name, 21);
        }
        let type_id = r.type_id;
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.name, image: r.image, category, price: to_plex(r.average), daily: false });
    }

    // Crates that traded on Serenity, with their splash art.
    let rows = crate::dao::store::catalogue::crates(db).await.map_err(db_err)?;
    for r in rows {
        let type_id = r.type_id;
        let image = if r.splash.is_empty() { r.image } else { r.splash };
        cat.offers.push(Offer { id: type_id as i64, type_id, quantity: 1, name: r.name, image, category: 4901, price: to_plex(r.average), daily: false });
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
    crate::dao::store::rewards::daily_gift(&c.db, c.login.user_id, DAILY_GIFT_OFFER as i32).await.map_err(db_err)
}
/// buy_offer(offer_id, currency, qty, from_character_id=, to_character_id=, ...) -> success
async fn buy_offer(c: &mut Client, call: &Call<'_>) -> CallResult {
    let offer_id = call.args.first().and_then(|v| v.as_int()).unwrap_or(0);
    let qty = call.args.get(2).and_then(|v| v.as_int()).unwrap_or(1).max(1);
    let gift_to = call.kwargs.get("to_character_id").and_then(|v| v.as_int());
    let cat = catalogue(c).await?;
    let Some(offer) = cat.offers.iter().find(|o| o.id == offer_id && !o.daily) else { return Ok(Value::Bool(false)) };
    let cost = offer.price * qty;
    let recipient = match gift_to {
        Some(ch) => match crate::dao::store::purchases::recipient(&c.db, ch).await.map_err(db_err)? {
            Some(user) => user,
            None => return Ok(Value::Bool(false)),
        },
        None => c.login.user_id,
    };
    let Some(balance) = crate::dao::store::purchases::debit(&c.db, c.login.user_id, cost).await.map_err(db_err)? else {
        info!(user = c.login.user_id, "store: cannot afford offer {offer_id} x{qty} ({cost} PLEX)");
        return Ok(Value::Bool(false));
    };
    crate::dao::store::purchases::purchase(&c.db, c.login.user_id, offer_id as i32, qty as i32, cost).await.map_err(db_err)?;
    grant_token(c, recipient, offer.type_id, offer.quantity * qty, "UI/VirtualGoodsStore/StoreName").await?;
    info!(user = c.login.user_id, "store: bought offer {offer_id} ({}) x{qty} for {cost} PLEX", offer.name);
    notify_plex_balance(c, balance);
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
    crate::dao::store::purchases::free_purchase(&c.db, c.login.user_id, offer_id as i32).await.map_err(db_err)?;
    grant_token(c, c.login.user_id, offer.type_id, offer.quantity, "UI/VirtualGoodsStore/StoreName").await?;
    c.notify("OnRedeemingQueueUpdated", vec![]);
    Ok(Value::Bool(true))
}

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
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

        _ => return None,
    })
}
