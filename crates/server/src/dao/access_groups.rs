//! Access-group persistence for `eve/client/script/ui/structure/accessGroups/`.
//! Uses `game.access_groups`, `game.access_group_members` and `game.access_group_logs`.
//! Membership policy and audit decisions belong to the calling service.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

#[derive(Clone)]
pub(crate) struct Group {
    pub group_id: i64, pub creator_id: i64, pub name: String, pub description: String,
}
impl Group {
    fn from_row(r: &Row) -> Self {
        Self { group_id: r.get("group_id"), creator_id: r.get("creator_id"), name: r.get("name"), description: r.get("description") }
    }
}
#[derive(Clone)]
pub(crate) struct Member {
    pub member_id: i64, pub membership_type: i32,
}
impl Member {
    fn from_row(r: &Row) -> Self {
        Self { member_id: r.get("member_id"), membership_type: r.get("membership_type") }
    }
}
#[derive(Clone)]
pub(crate) struct Log {
    pub log_date: i64, pub character_id: i64,
    pub action_id: i32, pub owner_id: Option<i64>, pub old_text: Option<String>,
    pub new_text: Option<String>, pub old_membership_type: Option<i32>, pub new_membership_type: Option<i32>,
}
impl Log {
    fn from_row(r: &Row) -> Self {
        Self { log_date: r.get("log_date"),
            character_id: r.get("character_id"), action_id: r.get("action_id"), owner_id: r.get("owner_id"),
            old_text: r.get("old_text"), new_text: r.get("new_text"), old_membership_type: r.get("old_membership_type"),
            new_membership_type: r.get("new_membership_type") }
    }
}
pub(crate) struct NewLog<'a> {
    pub group_id: i64, pub log_date: i64, pub character_id: i64, pub action_id: i32,
    pub owner_id: Option<i64>, pub old_text: Option<&'a str>, pub new_text: Option<&'a str>,
    pub old_membership_type: Option<i32>, pub new_membership_type: Option<i32>,
}
pub(crate) async fn groups(ex: &(impl Executor + ?Sized)) -> Result<Vec<Group>, Error> {
    Ok(ex.query("SELECT group_id, creator_id, name, description FROM game.access_groups ORDER BY group_id", &[]).await?.iter().map(Group::from_row).collect())
}
pub(crate) async fn group(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Group>, Error> {
    Ok(ex.query_opt("SELECT group_id, creator_id, name, description FROM game.access_groups WHERE group_id = $1", &[&id]).await?.as_ref().map(Group::from_row))
}
pub(crate) async fn members(ex: &(impl Executor + ?Sized), group: i64) -> Result<Vec<Member>, Error> {
    Ok(ex.query("SELECT member_id, membership_type FROM game.access_group_members WHERE group_id = $1 ORDER BY member_id", &[&group]).await?.iter().map(Member::from_row).collect())
}
pub(crate) async fn matching_members(ex: &(impl Executor + ?Sized), group: i64, entities: &[i64]) -> Result<Vec<Member>, Error> {
    Ok(ex.query("SELECT member_id, membership_type FROM game.access_group_members WHERE group_id = $1 AND member_id = ANY($2) ORDER BY member_id", &[&group, &entities]).await?.iter().map(Member::from_row).collect())
}
pub(crate) async fn create(ex: &(impl Executor + ?Sized), creator: i64, name: &str, description: &str) -> Result<Group, Error> {
    Ok(Group::from_row(&ex.query_one("INSERT INTO game.access_groups (creator_id, name, description) VALUES ($1, $2, $3) RETURNING group_id, creator_id, name, description", &[&creator, &name, &description]).await?))
}
pub(crate) async fn update_name(ex: &(impl Executor + ?Sized), id: i64, text: &str) -> Result<u64, Error> {
    ex.execute("UPDATE game.access_groups SET name = $2 WHERE group_id = $1", &[&id, &text]).await
}
pub(crate) async fn update_description(ex: &(impl Executor + ?Sized), id: i64, text: &str) -> Result<u64, Error> {
    ex.execute("UPDATE game.access_groups SET description = $2 WHERE group_id = $1", &[&id, &text]).await
}
pub(crate) async fn set_member(ex: &(impl Executor + ?Sized), group: i64, entity: i64, membership_type: i32) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.access_group_members (group_id, member_id, membership_type) VALUES ($1, $2, $3) ON CONFLICT (group_id, member_id) DO UPDATE SET membership_type = EXCLUDED.membership_type", &[&group, &entity, &membership_type]).await
}
pub(crate) async fn delete_member(ex: &(impl Executor + ?Sized), group: i64, entity: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.access_group_members WHERE group_id = $1 AND member_id = $2", &[&group, &entity]).await
}
pub(crate) async fn delete(ex: &(impl Executor + ?Sized), group: i64) -> Result<u64, Error> {
    ex.execute("DELETE FROM game.access_groups WHERE group_id = $1", &[&group]).await
}
pub(crate) async fn logs(ex: &(impl Executor + ?Sized), group: i64) -> Result<Vec<Log>, Error> {
    Ok(ex.query("SELECT log_date, character_id, action_id, owner_id, old_text, new_text, old_membership_type, new_membership_type FROM game.access_group_logs WHERE group_id = $1 ORDER BY log_date, log_id", &[&group]).await?.iter().map(Log::from_row).collect())
}
pub(crate) async fn insert_log(ex: &(impl Executor + ?Sized), log: &NewLog<'_>) -> Result<u64, Error> {
    ex.execute("INSERT INTO game.access_group_logs (group_id, log_date, character_id, action_id, owner_id, old_text, new_text, old_membership_type, new_membership_type) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&log.group_id, &log.log_date, &log.character_id, &log.action_id, &log.owner_id, &log.old_text, &log.new_text, &log.old_membership_type, &log.new_membership_type]).await
}
