//! Durable custom-room metadata and ACLs for `xmppchatclient/xmppchatsvc.py`
//! and `xmppchannelsettings.py`. Uses `game.chat_channels`,
//! `game.chat_channel_members`, and `game.chat_system_motds`. Occupants and
//! system-room admission are not stored; callers authorize system MOTD writes,
//! validate room identities, and own transactions and notifications.
//! Neutral (`none`) affiliation carries only a voice override; it never grants
//! membership or access, even when invited. Public access comes from the room.
//! Moderation deadlines are persisted with the ACL; callers schedule expiry.

use crate::db::{self, executor::Executor};
use tokio_postgres::{Row, types::{FromSql, Type}};

#[derive(Debug, Clone)]
pub(crate) struct Channel {
    pub name: String,
    pub category: String,
    pub display_name: String,
    pub owner_id: i64,
    pub public: bool,
    pub motd: String,
    pub moderated: bool,
    pub members_by_default: bool,
    pub private_low: Option<i64>,
    pub private_high: Option<i64>,
}

impl Channel {
    fn from_row(row: Row) -> Self {
        Self {
            name: row.get("name"),
            category: row.get("category"),
            display_name: row.get("display_name"),
            owner_id: row.get("owner_id"),
            public: row.get("public"),
            motd: row.get("motd"),
            moderated: row.get("moderated"),
            members_by_default: row.get("members_by_default"),
            private_low: row.get("private_low"),
            private_high: row.get("private_high"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Voice {
    Default,
    Muted,
    Voiced,
}

impl Voice {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Muted => "muted",
            Self::Voiced => "voiced",
        }
    }
}

impl<'a> FromSql<'a> for Voice {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        match <&str as FromSql>::from_sql(ty, raw)? {
            "default" => Ok(Self::Default),
            "muted" => Ok(Self::Muted),
            "voiced" => Ok(Self::Voiced),
            _ => Err("invalid chat voice".into()),
        }
    }
    fn accepts(ty: &Type) -> bool { <&str as FromSql>::accepts(ty) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Affiliation {
    None,
    Member,
    Admin,
    Outcast,
}

impl Affiliation {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Outcast => "outcast",
        }
    }
}

impl<'a> FromSql<'a> for Affiliation {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        match <&str as FromSql>::from_sql(ty, raw)? {
            "none" => Ok(Self::None),
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            "outcast" => Ok(Self::Outcast),
            _ => Err("invalid chat affiliation".into()),
        }
    }
    fn accepts(ty: &Type) -> bool { <&str as FromSql>::accepts(ty) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Restriction {
    Mute,
    Ban,
}

impl Restriction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Ban => "ban",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub character_id: i64,
    pub affiliation: Affiliation,
    pub invited: bool,
    pub voice: Voice,
    pub voice_expires_at: Option<i64>,
    pub ban_expires_at: Option<i64>,
}

impl Member {
    fn from_row(row: Row) -> Result<Self, db::Error> {
        Ok(Self {
            character_id: row.try_get("character_id")?,
            affiliation: row.try_get("affiliation")?,
            invited: row.try_get("invited")?,
            voice: row.try_get("voice")?,
            voice_expires_at: row.try_get("voice_expires_at")?,
            ban_expires_at: row.try_get("ban_expires_at")?,
        })
    }
}

struct SystemMotd {
    motd: String,
}

impl SystemMotd {
    fn from_row(row: Row) -> Self {
        Self { motd: row.get("motd") }
    }
}

pub(crate) async fn get(ex: &(impl Executor + ?Sized), name: &str) -> Result<Option<Channel>, db::Error> {
    Ok(ex.query_opt("SELECT * FROM game.chat_channels WHERE name = $1", &[&name]).await?.map(Channel::from_row))
}

/// Holds the room lock until the caller's transaction completes.
pub(crate) async fn get_for_update(ex: &(impl Executor + ?Sized), name: &str) -> Result<Option<Channel>, db::Error> {
    Ok(ex.query_opt("SELECT * FROM game.chat_channels WHERE name = $1 FOR UPDATE", &[&name]).await?.map(Channel::from_row))
}

pub(crate) async fn list_visible(ex: &(impl Executor + ?Sized), character_id: i64, now: i64) -> Result<Vec<Channel>, db::Error> {
    Ok(ex.query("SELECT c.* FROM game.chat_channels c LEFT JOIN game.chat_channel_members m ON m.channel_name = c.name AND m.character_id = $1::bigint WHERE $1::bigint > 0 AND (c.owner_id = $1::bigint OR ((m.ban_expires_at IS NULL OR m.ban_expires_at <= $2::bigint) AND m.affiliation IS DISTINCT FROM 'outcast')) AND ((c.category = 'private' AND NOT c.public AND c.private_low > 0 AND c.private_low < c.private_high AND c.owner_id IN (c.private_low, c.private_high) AND c.name = 'private_' || c.private_low::text || '_' || c.private_high::text AND $1::bigint IN (c.private_low, c.private_high)) OR (c.category = 'player' AND c.name ~ '^player_[1-9][0-9]*$' AND c.private_low IS NULL AND c.private_high IS NULL AND c.owner_id > 0 AND (c.owner_id = $1::bigint OR c.public OR m.affiliation IN ('member', 'admin')))) ORDER BY c.name", &[&character_id, &now]).await?.into_iter().map(Channel::from_row).collect())
}

pub(crate) async fn member(ex: &(impl Executor + ?Sized), name: &str, character_id: i64) -> Result<Option<Member>, db::Error> {
    ex.query_opt("SELECT character_id, affiliation, invited, voice, voice_expires_at, ban_expires_at FROM game.chat_channel_members WHERE channel_name = $1 AND character_id = $2", &[&name, &character_id]).await?.map(Member::from_row).transpose()
}

pub(crate) async fn members(ex: &(impl Executor + ?Sized), name: &str) -> Result<Vec<Member>, db::Error> {
    ex.query("SELECT character_id, affiliation, invited, voice, voice_expires_at, ban_expires_at FROM game.chat_channel_members WHERE channel_name = $1 ORDER BY character_id", &[&name]).await?.into_iter().map(Member::from_row).collect()
}

pub(crate) async fn create_player(ex: &(impl Executor + ?Sized), owner_id: i64, display_name: &str) -> Result<Channel, db::Error> {
    Ok(Channel::from_row(ex.query_one("INSERT INTO game.chat_channels (name, category, display_name, owner_id) VALUES ('player_' || nextval('game.chat_channel_id_seq')::text, 'player', $2, $1) RETURNING *", &[&owner_id, &display_name]).await?))
}

pub(crate) async fn create_private(ex: &(impl Executor + ?Sized), owner_id: i64, other_id: i64) -> Result<Channel, db::Error> {
    Ok(Channel::from_row(ex.query_one("INSERT INTO game.chat_channels (name, category, display_name, owner_id, private_low, private_high) VALUES ('private_' || LEAST($1::bigint, $2::bigint)::text || '_' || GREATEST($1::bigint, $2::bigint)::text, 'private', '', $1, LEAST($1::bigint, $2::bigint), GREATEST($1::bigint, $2::bigint)) ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name WHERE game.chat_channels.category = 'private' AND game.chat_channels.private_low = EXCLUDED.private_low AND game.chat_channels.private_high = EXCLUDED.private_high AND NOT game.chat_channels.public AND game.chat_channels.owner_id IN (game.chat_channels.private_low, game.chat_channels.private_high) RETURNING *", &[&owner_id, &other_id]).await?))
}

pub(crate) async fn may_manage(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.chat_channels c WHERE c.name = $1 AND c.category = 'player' AND (c.owner_id = $2::bigint OR EXISTS (SELECT 1 FROM game.chat_channel_members m WHERE m.channel_name = c.name AND m.character_id = $2::bigint AND m.affiliation = 'admin' AND m.voice <> 'muted' AND (m.voice_expires_at IS NULL OR m.voice_expires_at <= $3::bigint) AND (m.ban_expires_at IS NULL OR m.ban_expires_at <= $3::bigint)))", &[&name, &actor_id, &now]).await?.is_some())
}

pub(crate) async fn configure(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, public: bool, moderated: bool, members_by_default: bool) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.chat_channels c SET public = $3, moderated = $4, members_by_default = $5 WHERE c.name = $1 AND c.category = 'player' AND (c.owner_id = $2 OR EXISTS (SELECT 1 FROM game.chat_channel_members m WHERE m.channel_name = c.name AND m.character_id = $2 AND m.affiliation = 'admin'))", &[&name, &actor_id, &public, &moderated, &members_by_default]).await? != 0)
}

pub(crate) async fn rename(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, display_name: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.chat_channels c SET display_name = $3 WHERE c.name = $1 AND c.category = 'player' AND (c.owner_id = $2 OR EXISTS (SELECT 1 FROM game.chat_channel_members m WHERE m.channel_name = c.name AND m.character_id = $2 AND m.affiliation = 'admin'))", &[&name, &actor_id, &display_name]).await? != 0)
}

pub(crate) async fn set_motd(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, motd: &str) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.chat_channels c SET motd = $3 WHERE c.name = $1 AND c.category = 'player' AND (c.owner_id = $2 OR EXISTS (SELECT 1 FROM game.chat_channel_members m WHERE m.channel_name = c.name AND m.character_id = $2 AND m.affiliation = 'admin'))", &[&name, &actor_id, &motd]).await? != 0)
}

/// Transfers ownership and removes the new owner's redundant ACL atomically.
pub(crate) async fn transfer_owner(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, new_owner_id: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("WITH changed AS (UPDATE game.chat_channels SET owner_id = $3::bigint WHERE name = $1 AND category = 'player' AND owner_id = $2::bigint AND $3::bigint > 0 AND $3::bigint <> $2::bigint RETURNING name), removed AS (DELETE FROM game.chat_channel_members m USING changed c WHERE m.channel_name = c.name AND m.character_id = $3::bigint) SELECT name FROM changed", &[&name, &actor_id, &new_owner_id]).await?.is_some())
}

pub(crate) async fn set_member(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, character_id: i64, affiliation: Affiliation, invited: bool, voice: Voice) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.chat_channel_members (channel_name, character_id, affiliation, invited, voice) SELECT c.name, $3::bigint, $4, $5, $6 FROM game.chat_channels c WHERE c.name = $1 AND c.category = 'player' AND $3::bigint > 0 AND $3::bigint <> c.owner_id AND $4 IN ('none', 'member', 'admin', 'outcast') AND (c.owner_id = $2::bigint OR ($4 <> 'admin' AND EXISTS (SELECT 1 FROM game.chat_channel_members a WHERE a.channel_name = c.name AND a.character_id = $2::bigint AND a.affiliation = 'admin') AND NOT EXISTS (SELECT 1 FROM game.chat_channel_members t WHERE t.channel_name = c.name AND t.character_id = $3::bigint AND t.affiliation = 'admin'))) ON CONFLICT (channel_name, character_id) DO UPDATE SET affiliation = EXCLUDED.affiliation, invited = EXCLUDED.invited, voice = EXCLUDED.voice", &[&name, &actor_id, &character_id, &affiliation.as_str(), &invited, &voice.as_str()]).await? != 0)
}

/// The caller locks the room and authorizes the moderation operation.
pub(crate) async fn attach_expiry(ex: &(impl Executor + ?Sized), name: &str, character_id: i64, kind: Restriction, expires_at: i64, now: i64) -> Result<bool, db::Error> {
    let sql = match kind {
        Restriction::Mute => "UPDATE game.chat_channel_members SET voice = CASE WHEN voice = 'muted' THEN 'default' ELSE voice END, voice_expires_at = $3::bigint WHERE channel_name = $1 AND character_id = $2::bigint AND (voice = 'muted' OR voice_expires_at > $4::bigint)",
        Restriction::Ban => "UPDATE game.chat_channel_members SET invited = CASE WHEN affiliation = 'outcast' THEN false ELSE invited END, affiliation = CASE WHEN affiliation = 'outcast' THEN 'none' ELSE affiliation END, ban_expires_at = $3::bigint WHERE channel_name = $1 AND character_id = $2::bigint AND (affiliation = 'outcast' OR ban_expires_at > $4::bigint)",
    };
    Ok(ex.execute(sql, &[&name, &character_id, &expires_at, &now]).await? != 0)
}

/// Clears only the deadline, leaving independent plain restrictions intact.
pub(crate) async fn clear_expiry(ex: &(impl Executor + ?Sized), name: &str, character_id: i64, kind: Restriction) -> Result<bool, db::Error> {
    let sql = match kind {
        Restriction::Mute => "UPDATE game.chat_channel_members SET voice_expires_at = NULL WHERE channel_name = $1 AND character_id = $2::bigint AND voice_expires_at IS NOT NULL",
        Restriction::Ban => "UPDATE game.chat_channel_members SET ban_expires_at = NULL WHERE channel_name = $1 AND character_id = $2::bigint AND ban_expires_at IS NOT NULL",
    };
    Ok(ex.execute(sql, &[&name, &character_id]).await? != 0)
}

/// Settles only the still-current restriction whose exact deadline is due.
pub(crate) async fn settle_expiry(ex: &(impl Executor + ?Sized), name: &str, character_id: i64, kind: Restriction, expires_at: i64, now: i64) -> Result<bool, db::Error> {
    let sql = match kind {
        Restriction::Mute => "UPDATE game.chat_channel_members SET voice_expires_at = NULL WHERE channel_name = $1 AND character_id = $2::bigint AND voice_expires_at = $3::bigint AND voice_expires_at <= $4::bigint",
        Restriction::Ban => "UPDATE game.chat_channel_members SET ban_expires_at = NULL WHERE channel_name = $1 AND character_id = $2::bigint AND ban_expires_at = $3::bigint AND ban_expires_at <= $4::bigint",
    };
    Ok(ex.execute(sql, &[&name, &character_id, &expires_at, &now]).await? != 0)
}

pub(crate) async fn delete_member(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64, character_id: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.chat_channel_members t USING game.chat_channels c WHERE t.channel_name = c.name AND c.name = $1 AND c.category = 'player' AND t.character_id = $3 AND t.character_id <> c.owner_id AND (c.owner_id = $2 OR (t.affiliation <> 'admin' AND EXISTS (SELECT 1 FROM game.chat_channel_members a WHERE a.channel_name = c.name AND a.character_id = $2 AND a.affiliation = 'admin')))", &[&name, &actor_id, &character_id]).await? != 0)
}

pub(crate) async fn delete(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("DELETE FROM game.chat_channels c WHERE c.name = $1 AND (c.owner_id = $2 OR (c.category = 'player' AND EXISTS (SELECT 1 FROM game.chat_channel_members m WHERE m.channel_name = c.name AND m.character_id = $2 AND m.affiliation = 'admin')))", &[&name, &actor_id]).await? != 0)
}

pub(crate) async fn get_system_motd(ex: &(impl Executor + ?Sized), name: &str) -> Result<Option<String>, db::Error> {
    Ok(ex.query_opt("SELECT motd FROM game.chat_system_motds WHERE channel_name = $1", &[&name]).await?.map(SystemMotd::from_row).map(|row| row.motd))
}

/// The service must authorize the caller against the owning corporation or alliance.
pub(crate) async fn set_system_motd(ex: &(impl Executor + ?Sized), name: &str, motd: &str) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.chat_system_motds (channel_name, motd) VALUES ($1, $2) ON CONFLICT (channel_name) DO UPDATE SET motd = EXCLUDED.motd", &[&name, &motd]).await?;
    Ok(())
}
