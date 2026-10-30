use super::*;

#[test]
fn loot_rejects_unpublished_null_and_other_categories() {
    assert_eq!(loot_category(Some(true), Some(CATEGORY_MODULE)), Some(CATEGORY_MODULE));
    assert_eq!(loot_category(Some(true), Some(CATEGORY_CHARGE)), Some(CATEGORY_CHARGE));
    assert_eq!(loot_category(Some(false), Some(CATEGORY_MODULE)), None);
    assert_eq!(loot_category(None, Some(CATEGORY_CHARGE)), None);
    assert_eq!(loot_category(Some(true), None), None);
    assert_eq!(loot_category(Some(true), Some(0)), None);
}
