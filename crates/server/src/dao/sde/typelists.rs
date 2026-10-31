//! Type-list projection for direct-trade restrictions from `sde_native.typelist`.
//! The client expands lists in `evetypes.GetTypeIDsFromTypeList`.
//! Other lists and fields are not loaded.
use crate::db::{executor::Executor, Error};

pub(crate) struct TypeList {
    pub(crate) included_types: Vec<i32>,
    pub(crate) included_groups: Vec<i32>,
    pub(crate) included_categories: Vec<i32>,
    pub(crate) excluded_types: Vec<i32>,
    pub(crate) excluded_groups: Vec<i32>,
    pub(crate) excluded_categories: Vec<i32>,
}

pub(crate) async fn load(ex: &(impl Executor + ?Sized)) -> Result<Vec<TypeList>, Error> {
    let rows = ex.query("SELECT ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'includedTypeIDs', '[]'::jsonb))) AS included_types, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'includedGroupIDs', '[]'::jsonb))) AS included_groups, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'includedCategoryIDs', '[]'::jsonb))) AS included_categories, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'excludedTypeIDs', '[]'::jsonb))) AS excluded_types, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'excludedGroupIDs', '[]'::jsonb))) AS excluded_groups, ARRAY(SELECT value::int FROM jsonb_array_elements_text(COALESCE(data->'excludedCategoryIDs', '[]'::jsonb))) AS excluded_categories FROM sde_native.typelist WHERE key IN ('36', '142')", &[]).await?;
    Ok(rows.iter().map(|row| TypeList { included_types: row.get("included_types"), included_groups: row.get("included_groups"), included_categories: row.get("included_categories"), excluded_types: row.get("excluded_types"), excluded_groups: row.get("excluded_groups"), excluded_categories: row.get("excluded_categories") }).collect())
}
