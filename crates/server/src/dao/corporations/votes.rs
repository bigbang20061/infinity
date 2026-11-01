//! Informational general vote persistence for `eve/client/script/ui/shared/neocom/corporation/voting`.
//! Uses corporation membership, share holdings, vote cases, snapshots and ballots.
//! Sanctioned actions, authorization policy, transactions and notifications are not handled here.
use crate::db::{self, executor::Executor};

pub(crate) struct Case { pub(crate) id: i64, pub(crate) corporation_id: i64, pub(crate) creator_id: i64, pub(crate) title: String, pub(crate) description: String, pub(crate) starts: i64, pub(crate) ends: i64, pub(crate) closed_at: Option<i64> }
impl Case { fn from_row(r: &tokio_postgres::Row) -> Self { Self { id: r.get("id"), corporation_id: r.get("corporation_id"), creator_id: r.get("creator_id"), title: r.get("title"), description: r.get("description"), starts: r.get("starts"), ends: r.get("ends"), closed_at: r.get("closed_at") } } }
pub(crate) struct OptionRow { pub(crate) option_id: i32, pub(crate) text: String, pub(crate) votes_for: i64 }
impl OptionRow { fn from_row(r: &tokio_postgres::Row) -> Self { Self { option_id: r.get("option_id"), text: r.get("text"), votes_for: r.get("votes_for") } } }
pub(crate) struct Ballot { pub(crate) character_id: i64, pub(crate) option_id: i32, pub(crate) weight: i64 }
impl Ballot { fn from_row(r: &tokio_postgres::Row) -> Self { Self { character_id: r.get("character_id"), option_id: r.get("option_id"), weight: r.get("weight") } } }
pub(crate) struct EligibleHolder { pub(crate) holder_id: i64 }
impl EligibleHolder { fn from_row(r: &tokio_postgres::Row) -> Self { Self { holder_id: r.get("holder_id") } } }
struct Number { value: i64 }
impl Number { fn from_row(r: &tokio_postgres::Row) -> Self { Self { value: r.get("value") } } }

pub(crate) async fn create(ex: &(impl Executor + ?Sized), issuer: i64, creator: i64, title: &str, description: &str, starts: i64, ends: i64) -> Result<i64, db::Error> {
    Ok(Number::from_row(&ex.query_one("INSERT INTO game.corp_vote_cases (corporation_id,creator_id,title,description,starts,ends) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id AS value", &[&issuer,&creator,&title,&description,&starts,&ends]).await?).value)
}
pub(crate) async fn add_option(ex: &(impl Executor + ?Sized), case: i64, option_id: i32, text: &str) -> Result<(), db::Error> {
    ex.execute("INSERT INTO game.corp_vote_options (case_id,option_id,text) VALUES ($1,$2,$3)", &[&case,&option_id,&text]).await?; Ok(())
}
pub(crate) async fn snapshot(ex: &(impl Executor + ?Sized), case: i64, issuer: i64) -> Result<u64, db::Error> {
    ex.execute("INSERT INTO game.corp_vote_snapshot (case_id,holder_id,shares) SELECT $1,shareholder_id,shares FROM game.corp_shares WHERE corporation_id=$2 AND shareholder_id<>$2 AND shares>0 ORDER BY shareholder_id ON CONFLICT (case_id,holder_id) DO NOTHING", &[&case,&issuer]).await
}
pub(crate) async fn case(ex: &(impl Executor + ?Sized), case_id: i64) -> Result<Option<Case>, db::Error> {
    Ok(ex.query_opt("SELECT id,corporation_id,creator_id,title,description,starts,ends,closed_at FROM game.corp_vote_cases WHERE id=$1", &[&case_id]).await?.as_ref().map(Case::from_row))
}
pub(crate) async fn lock_case(ex: &(impl Executor + ?Sized), case_id: i64) -> Result<Option<Case>, db::Error> {
    Ok(ex.query_opt("SELECT id,corporation_id,creator_id,title,description,starts,ends,closed_at FROM game.corp_vote_cases WHERE id=$1 FOR UPDATE", &[&case_id]).await?.as_ref().map(Case::from_row))
}
pub(crate) async fn cases(ex: &(impl Executor + ?Sized), issuer: i64, now: i64, closed_limit: i64) -> Result<Vec<Case>, db::Error> {
    Ok(ex.query("SELECT * FROM (SELECT id,corporation_id,creator_id,title,description,starts,ends,closed_at FROM game.corp_vote_cases WHERE corporation_id=$1 AND closed_at IS NULL AND ends>$2 UNION ALL (SELECT id,corporation_id,creator_id,title,description,starts,ends,closed_at FROM game.corp_vote_cases WHERE corporation_id=$1 AND (closed_at IS NOT NULL OR ends<=$2) ORDER BY COALESCE(closed_at,ends) DESC,id DESC LIMIT $3)) c ORDER BY starts DESC,id DESC", &[&issuer,&now,&closed_limit]).await?.iter().map(Case::from_row).collect())
}
pub(crate) async fn open_count(ex: &(impl Executor + ?Sized), issuer: i64, now: i64) -> Result<i64, db::Error> {
    Ok(Number::from_row(&ex.query_one("SELECT count(*) AS value FROM game.corp_vote_cases WHERE corporation_id=$1 AND closed_at IS NULL AND ends>$2", &[&issuer,&now]).await?).value)
}
pub(crate) async fn options(ex: &(impl Executor + ?Sized), case: i64) -> Result<Vec<OptionRow>, db::Error> {
    Ok(ex.query("SELECT o.option_id,o.text,COALESCE(sum(s.shares),0)::bigint AS votes_for FROM game.corp_vote_options o LEFT JOIN game.corp_vote_ballots b ON b.case_id=o.case_id AND b.option_id=o.option_id LEFT JOIN game.corp_vote_claims c ON c.case_id=b.case_id AND c.character_id=b.character_id LEFT JOIN game.corp_vote_snapshot s ON s.case_id=c.case_id AND s.holder_id=c.holder_id WHERE o.case_id=$1 GROUP BY o.option_id,o.text ORDER BY o.option_id", &[&case]).await?.iter().map(OptionRow::from_row).collect())
}
pub(crate) async fn ballot(ex: &(impl Executor + ?Sized), case: i64, actor: i64) -> Result<Option<Ballot>, db::Error> {
    Ok(ex.query_opt("SELECT b.character_id,b.option_id,COALESCE(sum(s.shares),0)::bigint AS weight FROM game.corp_vote_ballots b LEFT JOIN game.corp_vote_claims c ON c.case_id=b.case_id AND c.character_id=b.character_id LEFT JOIN game.corp_vote_snapshot s ON s.case_id=c.case_id AND s.holder_id=c.holder_id WHERE b.case_id=$1 AND b.character_id=$2 GROUP BY b.character_id,b.option_id", &[&case,&actor]).await?.as_ref().map(Ballot::from_row))
}
pub(crate) async fn eligible_holders(ex: &(impl Executor + ?Sized), case: i64, actor: i64) -> Result<Vec<EligibleHolder>, db::Error> {
    Ok(ex.query("SELECT s.holder_id FROM game.corp_vote_snapshot s LEFT JOIN game.corp_vote_claims cl ON cl.case_id=s.case_id AND cl.holder_id=s.holder_id WHERE s.case_id=$1 AND cl.holder_id IS NULL AND (s.holder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$2)) ORDER BY s.holder_id", &[&case,&actor]).await?.iter().map(EligibleHolder::from_row).collect())
}
pub(crate) async fn insert_ballot(ex: &(impl Executor + ?Sized), case: i64, actor: i64, option: i32, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_vote_ballots (case_id,character_id,option_id,created) SELECT c.id,$2,o.option_id,$4 FROM game.corp_vote_cases c JOIN game.corp_vote_options o ON o.case_id=c.id AND o.option_id=$3 WHERE c.id=$1 AND c.closed_at IS NULL AND c.ends>$4 ON CONFLICT (case_id,character_id) DO NOTHING", &[&case,&actor,&option,&now]).await? != 0)
}
pub(crate) async fn claim_holder(ex: &(impl Executor + ?Sized), case: i64, holder: i64, actor: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("INSERT INTO game.corp_vote_claims (case_id,holder_id,character_id) SELECT s.case_id,s.holder_id,$3 FROM game.corp_vote_snapshot s WHERE s.case_id=$1 AND s.holder_id=$2 AND (s.holder_id=$3 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$3)) ON CONFLICT (case_id,holder_id) DO NOTHING", &[&case,&holder,&actor]).await? != 0)
}
pub(crate) async fn close(ex: &(impl Executor + ?Sized), case: i64, now: i64) -> Result<bool, db::Error> {
    Ok(ex.execute("UPDATE game.corp_vote_cases SET closed_at=$2 WHERE id=$1 AND closed_at IS NULL AND ends<=$2", &[&case,&now]).await? != 0)
}
pub(crate) async fn can_view(ex: &(impl Executor + ?Sized), issuer: i64, actor: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 WHERE EXISTS (SELECT 1 FROM game.corp_shares s WHERE s.corporation_id=$1 AND s.shares>0 AND (s.shareholder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.shareholder_id AND NOT c.deleted AND c.ceo_id=$2))) OR EXISTS (SELECT 1 FROM game.corp_vote_cases v JOIN game.corp_vote_snapshot s ON s.case_id=v.id WHERE v.corporation_id=$1 AND (s.holder_id=$2 OR EXISTS (SELECT 1 FROM game.corporations c WHERE c.corporation_id=s.holder_id AND NOT c.deleted AND c.ceo_id=$2))) OR EXISTS (SELECT 1 FROM game.corp_vote_cases v JOIN game.corp_vote_ballots b ON b.case_id=v.id WHERE v.corporation_id=$1 AND b.character_id=$2)", &[&issuer,&actor]).await?.is_some())
}
pub(crate) async fn observers(ex: &(impl Executor + ?Sized), case: i64) -> Result<Vec<i64>, db::Error> {
    Ok(ex.query("SELECT character_id AS value FROM game.characters WHERE NOT deleted AND corporation_id=(SELECT corporation_id FROM game.corp_vote_cases WHERE id=$1) UNION SELECT ch.character_id AS value FROM game.corp_vote_snapshot s JOIN game.characters ch ON ch.character_id=s.holder_id AND NOT ch.deleted WHERE s.case_id=$1 UNION SELECT c.ceo_id AS value FROM game.corp_vote_snapshot s JOIN game.corporations c ON c.corporation_id=s.holder_id AND NOT c.deleted JOIN game.characters ch ON ch.character_id=c.ceo_id AND NOT ch.deleted WHERE s.case_id=$1 UNION SELECT character_id AS value FROM game.corp_vote_ballots WHERE case_id=$1 ORDER BY value", &[&case]).await?.iter().map(|r| Number::from_row(r).value).collect())
}
