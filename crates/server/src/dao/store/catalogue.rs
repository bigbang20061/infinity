//! Store catalogue queries joining retail prices with client SDE type data.
//! Used by vgs/client/vgsClient.py; the live store catalogue was not shipped.
use crate::db::{executor::Executor, Error};
pub(crate) const TYPE_INFO_JOINS: &str = "
    LEFT JOIN sde_native.types nt ON nt.key = t.type_id::text
    LEFT JOIN sde_native.localization_fsd_zh ln ON ln.key = nt.data->>'typeNameID'
    LEFT JOIN sde_native.iconids ic ON ic.key = nt.data->>'iconID'";
pub(crate) const TYPE_INFO_COLUMNS: &str = "t.type_id, COALESCE(ln.data->>'text', t.name) AS name, COALESCE(ic.data->>'iconFile', '') AS image";
struct Price { average:f64 }
impl Price { fn from_row(r:&tokio_postgres::Row)->Self { Self { average:r.get("average") } } }
pub(crate) struct Service { pub name:String,pub image:String,pub average:Option<f64> }
impl Service { fn from_row(r:&tokio_postgres::Row)->Self { Self { name:r.get("name"),image:r.get("image"),average:r.get("average") } } }
pub(crate) struct Skin { pub type_id:i32,pub name:String,pub image:String,pub average:f64,pub race_id:i32 }
impl Skin { fn from_row(r:&tokio_postgres::Row)->Self { Self {type_id:r.get("type_id"),name:r.get("name"),image:r.get("image"),average:r.get("average"),race_id:r.get("race_id")} } }
pub(crate) struct Apparel { pub type_id:i32,pub name:String,pub image:String,pub average:f64,pub group_id:i64,pub group_name:String }
impl Apparel { fn from_row(r:&tokio_postgres::Row)->Self { Self {type_id:r.get("type_id"),name:r.get("name"),image:r.get("image"),average:r.get("average"),group_id:r.get("group_id"),group_name:r.get("group_name")} } }
pub(crate) struct Crate { pub type_id:i32,pub name:String,pub image:String,pub average:f64,pub splash:String }
impl Crate { fn from_row(r:&tokio_postgres::Row)->Self { Self {type_id:r.get("type_id"),name:r.get("name"),image:r.get("image"),average:r.get("average"),splash:r.get("splash")} } }
pub(crate) async fn plex_price(ex:&impl Executor,type_id:i32)->Result<Option<f64>,Error> { Ok(ex.query_opt("SELECT average FROM game.retail_prices WHERE type_id = $1",&[&type_id]).await?.map(|r|Price::from_row(&r).average)) }
pub(crate) async fn service(ex:&impl Executor,type_id:i32)->Result<Service,Error> { Ok(Service::from_row(&ex.query_one(&format!("SELECT {TYPE_INFO_COLUMNS}, r.average FROM sde.inv_types t {TYPE_INFO_JOINS}
                          LEFT JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0 WHERE t.type_id = $1"),&[&type_id]).await?)) }
pub(crate) async fn skins(ex:&impl Executor)->Result<Vec<Skin>,Error> { Ok(ex.query(&format!("SELECT {TYPE_INFO_COLUMNS}, r.average, COALESCE(hull.race_id, 0) AS race_id
                 FROM sde_native.skinlicenses l
                 JOIN sde_native.skins s ON s.key = l.data->>'skinID'
                 JOIN sde.inv_types t ON t.type_id = l.key::int
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 LEFT JOIN sde.inv_types hull ON hull.type_id = (s.data->'types'->>0)::int
                 {TYPE_INFO_JOINS}
                 WHERE t.published AND (s.data->>'visibleSerenity')::boolean AND (l.data->>'duration')::int = -1
                 ORDER BY t.type_id"),&[]).await?.iter().map(Skin::from_row).collect()) }
pub(crate) async fn apparel(ex:&impl Executor,name:&str)->Result<Vec<Apparel>,Error> { Ok(ex.query(&format!("SELECT {TYPE_INFO_COLUMNS}, r.average, sub.key::bigint AS group_id, COALESCE(sn.data->>'text', $1::text) AS group_name
                 FROM sde.inv_types t
                 JOIN sde.inv_categories c ON c.category_id = t.category_id AND c.name = 'Apparel'
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 JOIN sde_native.marketgroups mg ON mg.key = t.market_group_id::text
                 JOIN sde_native.marketgroups sub ON sub.key = COALESCE(mg.data->>'parentGroupID', mg.key)
                 LEFT JOIN sde_native.localization_fsd_zh sn ON sn.key = sub.data->>'nameID'
                 {TYPE_INFO_JOINS}
                 WHERE t.published
                 ORDER BY t.type_id"),&[&name]).await?.iter().map(Apparel::from_row).collect()) }
pub(crate) async fn crates(ex:&impl Executor)->Result<Vec<Crate>,Error> { Ok(ex.query(&format!("SELECT {TYPE_INFO_COLUMNS}, r.average, COALESCE(cr.data->'staticSplash'->>'resPath', '') AS splash
                 FROM sde_native.crates cr
                 JOIN sde.inv_types t ON t.type_id = (cr.data->>'typeID')::int
                 JOIN game.retail_prices r ON r.type_id = t.type_id AND r.average > 0
                 {TYPE_INFO_JOINS}
                 WHERE t.published
                 ORDER BY t.type_id"),&[]).await?.iter().map(Crate::from_row).collect()) }
