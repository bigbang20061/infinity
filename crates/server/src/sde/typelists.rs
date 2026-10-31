//! Trade restrictions from native type lists, mirroring `inventoryrestrictions/common/checks.py`.
//! List 36 and list 142 use client type-list inclusion/exclusion precedence; other lists are not loaded.
use crate::{dao::sde::typelists as dao, db::Db};

pub(crate) struct TypeLists(Vec<dao::TypeList>);

impl TypeLists {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        Ok(Self(dao::load(db).await?))
    }

    pub(crate) fn is_tradable(&self, type_id: i32, group_id: Option<i32>, category_id: Option<i32>) -> bool {
        !self.0.iter().any(|list| {
            let in_category = category_id.is_some_and(|id| list.included_categories.contains(&id) && !list.excluded_categories.contains(&id));
            let in_group = group_id.is_some_and(|id| list.included_groups.contains(&id));
            let excluded_group = group_id.is_some_and(|id| list.excluded_groups.contains(&id));
            ((in_category || in_group) && !excluded_group || list.included_types.contains(&type_id)) && !list.excluded_types.contains(&type_id)
        })
    }
}
