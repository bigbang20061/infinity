//! Public gateway request handlers, keyed by request message full name.
//! Returning `Ok(None)` makes the gateway answer with an empty matching response.

use super::{message, Caller, Gateway};
use crate::cdn;
use anyhow::{Context, Result};
use bytes::Bytes;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage, Value as PV};

pub async fn handle(gw: &Gateway, caller: &Caller, name: &str, req: &DynamicMessage) -> Result<Option<DynamicMessage>> {
    match name {
        "eve_public.achievement.definition.api.cdn.GetLatestRequest" => checkpoint(gw, name, "achievements").map(Some),
        "eve_public.cosmetic.character.title.definition.api.GetLatestRequest" => checkpoint(gw, name, "titles").map(Some),
        "eve_public.space.api.cdn.GetUpdateRequest" => checkpoint(gw, name, "maptags").map(Some),
        "eve_public.career.goal.api.GetDefinitionsRequest" => career_goal_definitions().map(Some),
        "eve_public.career.goal.api.GetAllRequest" => career_goal_states().map(Some),
        "eve_public.pirate.corruption.api.GetStageThresholdsRequest" | "eve_public.pirate.suppression.api.GetStageThresholdsRequest" => {
            stage_thresholds(name).map(Some)
        }
        "eve_public.character.GetNamesByUserRequest" => names_by_user(gw, caller).await.map(Some),
        "eve_public.plex.vault.api.BalanceRequest" => plex_balance(gw, caller).await.map(Some),
        n if n.starts_with("eve_public.character.skill.plan.") && !n.contains(".milestone.") => skill_plan(gw, caller, n, req).await,
        _ => Ok(None),
    }
}

fn db(gw: &Gateway) -> Result<&tokio_postgres::Client> {
    Ok(&gw.db.as_ref().context("gateway has no database")?.client)
}

fn msg_field(m: &DynamicMessage, field: &str) -> Option<DynamicMessage> {
    m.get_field_by_name(field)?.as_message().cloned()
}

fn str_field(m: &DynamicMessage, field: &str) -> String {
    m.get_field_by_name(field).and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

/// `eve_public.<x>.Identifier{sequential}`
fn identifier(parent: &MessageDescriptor, field: &str, sequential: PV) -> Result<DynamicMessage> {
    let mut id = child(parent, field)?;
    set(&mut id, "sequential", sequential)?;
    Ok(id)
}

/// Insurgency stages: five equal steps of corruption / suppression.
fn stage_thresholds(request: &str) -> Result<DynamicMessage> {
    let mut rsp = response_for(request)?;
    let mut thresholds = vec![];
    for k in 1..=5u64 {
        let mut f = child(&rsp.descriptor(), "thresholds")?;
        set(&mut f, "numerator", PV::U64(k))?;
        set(&mut f, "denominator", PV::U64(5))?;
        thresholds.push(PV::Message(f));
    }
    set(&mut rsp, "thresholds", PV::List(thresholds))?;
    Ok(rsp)
}

/// The account's characters (character rename search).
async fn names_by_user(gw: &Gateway, caller: &Caller) -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.character.GetNamesByUserRequest")?;
    let user = caller.user_id.context("no user")?;
    let rows = db(gw)?
        .query("SELECT character_id, name FROM game.characters WHERE user_id = $1 AND NOT deleted ORDER BY character_id", &[&user])
        .await?;
    let mut out = vec![];
    for r in rows {
        let mut entry = child(&rsp.descriptor(), "characters")?;
        let id = identifier(&entry.descriptor(), "identifier", PV::U32(r.get::<_, i64>(0) as u32))?;
        set(&mut entry, "identifier", PV::Message(id))?;
        set(&mut entry, "name", PV::String(r.get(1)))?;
        out.push(PV::Message(entry));
    }
    set(&mut rsp, "characters", PV::List(out))?;
    Ok(rsp)
}

/// The account's PLEX vault (crate::services::store), in hundredths of a PLEX.
async fn plex_balance(gw: &Gateway, caller: &Caller) -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.plex.vault.api.BalanceRequest")?;
    let user = caller.user_id.context("no user")?;
    let plex = crate::db::Db::plex_balance(db(gw)?, user).await?;
    let mut balance = child(&rsp.descriptor(), "balance")?;
    set(&mut balance, "total_in_cents", PV::I64(plex * 100))?;
    set(&mut rsp, "balance", PV::Message(balance))?;
    Ok(rsp)
}

// ---- personal skill plans (skills/skillplan/grpc/message_bus/skillPlanRequestsMessenger.py) ----

fn new_uuid() -> Vec<u8> {
    let mut id = [crate::handshake::rand_i64().to_le_bytes(), crate::handshake::rand_i64().to_le_bytes()].concat();
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

/// `skill_plan.uuid` of a request.
fn plan_id(req: &DynamicMessage) -> Option<Vec<u8>> {
    msg_field(req, "skill_plan")?.get_field_by_name("uuid")?.as_bytes().map(|b| b.to_vec())
}

/// (skill type IDs, levels) of repeated `eve_public.skill.plan.SkillRequirement`.
fn requirements(m: &DynamicMessage, field: &str) -> (Vec<i32>, Vec<i32>) {
    let (mut skills, mut levels) = (vec![], vec![]);
    if let Some(list) = m.get_field_by_name(field) {
        for r in list.as_list().unwrap_or_default().iter().filter_map(|v| v.as_message()) {
            let skill = msg_field(r, "skill_type").and_then(|t| t.get_field_by_name("sequential")?.as_u64()).unwrap_or(0);
            let level = r.get_field_by_name("level").and_then(|v| v.as_u32()).unwrap_or(0);
            skills.push(skill as i32);
            levels.push(level as i32);
        }
    }
    (skills, levels)
}

/// An `eve_public.skill.plan.Attributes` for `desc.field`.
fn plan_attributes(desc: &MessageDescriptor, field: &str, row: &tokio_postgres::Row) -> Result<DynamicMessage> {
    let mut attrs = child(desc, field)?;
    set(&mut attrs, "name", PV::String(row.get(0)))?;
    set(&mut attrs, "description", PV::String(row.get(1)))?;
    let (skills, levels): (Vec<i32>, Vec<i32>) = (row.get(2), row.get(3));
    let mut reqs = vec![];
    for (skill, level) in skills.iter().zip(&levels) {
        let mut r = child(&attrs.descriptor(), "skill_requirements")?;
        let id = identifier(&r.descriptor(), "skill_type", PV::U64(*skill as u64))?;
        set(&mut r, "skill_type", PV::Message(id))?;
        set(&mut r, "level", PV::U32(*level as u32))?;
        reqs.push(PV::Message(r));
    }
    set(&mut attrs, "skill_requirements", PV::List(reqs))?;
    Ok(attrs)
}

fn plan_identifier(desc: &MessageDescriptor, field: &str, id: Vec<u8>) -> Result<DynamicMessage> {
    let mut m = child(desc, field)?;
    set(&mut m, "uuid", PV::Bytes(Bytes::from(id)))?;
    Ok(m)
}

const PLAN_COLUMNS: &str = "name, description, skill_ids, levels, plan_id";

async fn skill_plan(gw: &Gateway, caller: &Caller, name: &str, req: &DynamicMessage) -> Result<Option<DynamicMessage>> {
    let me = caller.character_id.context("no character")?;
    let db = db(gw)?;
    let mut rsp = response_for(name)?;
    let desc = rsp.descriptor();
    let method = name.rsplit('.').next().unwrap_or_default();
    let id = plan_id(req);
    match method {
        "CreateRequest" => {
            let plan = msg_field(req, "skill_plan").context("no skill_plan")?;
            let (skills, levels) = requirements(&plan, "skill_requirements");
            let new = new_uuid();
            db.execute(
                "INSERT INTO game.skill_plans (plan_id, character_id, name, description, skill_ids, levels) VALUES ($1, $2, $3, $4, $5, $6)",
                &[&new, &me, &str_field(&plan, "name"), &str_field(&plan, "description"), &skills, &levels],
            )
            .await?;
            set(&mut rsp, "skill_plan", PV::Message(plan_identifier(&desc, "skill_plan", new)?))?;
        }
        "GetAllRequest" => {
            let rows = db.query("SELECT plan_id FROM game.skill_plans WHERE character_id = $1 ORDER BY created_at", &[&me]).await?;
            let ids = rows.iter().map(|r| plan_identifier(&desc, "skill_plans", r.get(0)).map(PV::Message)).collect::<Result<Vec<_>>>()?;
            set(&mut rsp, "skill_plans", PV::List(ids))?;
        }
        // Shared plans are looked up by id regardless of owner.
        "GetRequest" | "GetSharedRequest" => {
            let owner = if method == "GetRequest" { Some(me) } else { None };
            let row = db
                .query_opt(
                    &format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE plan_id = $1 AND ($2::bigint IS NULL OR character_id = $2)"),
                    &[&id.context("no skill_plan")?, &owner],
                )
                .await?;
            if let Some(row) = row {
                set(&mut rsp, "skill_plan", PV::Message(plan_attributes(&desc, "skill_plan", &row)?))?;
            }
        }
        "GetActiveRequest" => {
            let row = db.query_opt(&format!("SELECT {PLAN_COLUMNS} FROM game.skill_plans WHERE character_id = $1 AND active"), &[&me]).await?;
            // No active plan: the all-zero uuid.
            let plan = row.as_ref().map(|r| r.get::<_, Vec<u8>>(4)).unwrap_or_else(|| vec![0u8; 16]);
            set(&mut rsp, "skill_plan", PV::Message(plan_identifier(&desc, "skill_plan", plan)?))?;
            if let Some(row) = row {
                set(&mut rsp, "skill_plan_info", PV::Message(plan_attributes(&desc, "skill_plan_info", &row)?))?;
            }
        }
        "SetActiveRequest" => {
            db.execute("UPDATE game.skill_plans SET active = (plan_id = $2) WHERE character_id = $1", &[&me, &id.unwrap_or_default()]).await?;
        }
        "DeleteRequest" => {
            db.execute("DELETE FROM game.skill_plans WHERE character_id = $1 AND plan_id = $2", &[&me, &id.context("no skill_plan")?]).await?;
        }
        "SetNameRequest" | "SetDescriptionRequest" => {
            let column = if method == "SetNameRequest" { "name" } else { "description" };
            db.execute(
                &format!("UPDATE game.skill_plans SET {column} = $3 WHERE character_id = $1 AND plan_id = $2"),
                &[&me, &id.context("no skill_plan")?, &str_field(req, column)],
            )
            .await?;
        }
        "SetSkillRequirementsRequest" => {
            let (skills, levels) = requirements(req, "requirements");
            db.execute(
                "UPDATE game.skill_plans SET skill_ids = $3, levels = $4 WHERE character_id = $1 AND plan_id = $2",
                &[&me, &id.context("no skill_plan")?, &skills, &levels],
            )
            .await?;
        }
        _ => return Ok(None),
    }
    Ok(Some(rsp))
}

fn response_for(request: &str) -> Result<DynamicMessage> {
    let name = request.strip_suffix("Request").map(|s| format!("{s}Response")).context("not a request")?;
    Ok(DynamicMessage::new(message(&name).with_context(|| format!("no {name}"))?))
}

/// Message type of `field` in `desc` (singular, repeated or map value alike).
fn child(desc: &MessageDescriptor, field: &str) -> Result<DynamicMessage> {
    let f = desc.get_field_by_name(field).with_context(|| format!("{} has no {field}", desc.full_name()))?;
    Ok(DynamicMessage::new(f.kind().as_message().with_context(|| format!("{field} is not a message"))?.clone()))
}

fn set(m: &mut DynamicMessage, field: &str, v: PV) -> Result<()> {
    m.try_set_field_by_name(field, v).with_context(|| format!("setting {}.{field}", m.descriptor().full_name()))
}

/// `{checkpoint: {url, version, crc: {crc}}}` pointing at crate::cdn content.
fn checkpoint(gw: &Gateway, request: &str, content: &str) -> Result<DynamicMessage> {
    let (data, (major, minor, patch)) = cdn::content(content).context("unknown content")?;
    let mut rsp = response_for(request)?;
    let mut cp = child(&rsp.descriptor(), "checkpoint")?;
    set(&mut cp, "url", PV::String(format!("{}/content/{content}", gw.cdn_url)))?;
    let mut version = child(&cp.descriptor(), "version")?;
    set(&mut version, "major", PV::U32(major))?;
    set(&mut version, "minor", PV::U32(minor))?;
    set(&mut version, "patch", PV::U32(patch))?;
    set(&mut cp, "version", PV::Message(version))?;
    let mut crc = child(&cp.descriptor(), "crc")?;
    set(&mut crc, "crc", PV::U32(cdn::crc32(data)))?;
    set(&mut cp, "crc", PV::Message(crc))?;
    set(&mut rsp, "checkpoint", PV::Message(cp))?;
    tracing::debug!("gateway: checkpoint {content} ({} bytes) for {request}", data.len());
    Ok(rsp)
}

/// eve_public.career.goal.Attributes.Career for the four career paths.
const CAREERS: [i32; 4] = [1, 2, 3, 4];

/// Career goal ids: one top-level goal per career path (careergoals/client/goal_data_controller.py
/// needs `career_path_goal_definition` for every career).
fn career_goal_uuid(career: i32) -> Bytes {
    let mut id = [0u8; 16];
    id[6] = 0x40;
    id[8] = 0x80;
    id[15] = career as u8;
    Bytes::copy_from_slice(&id)
}

fn goal_identifier(entry: &DynamicMessage, career: i32) -> Result<DynamicMessage> {
    let mut goal = child(&entry.descriptor(), "goal")?;
    set(&mut goal, "uuid", PV::Bytes(career_goal_uuid(career)))?;
    Ok(goal)
}

fn career_goal_definitions() -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.career.goal.api.GetDefinitionsRequest")?;
    let mut goals = Vec::new();
    for career in CAREERS {
        let mut entry = child(&rsp.descriptor(), "goals")?;
        let goal = goal_identifier(&entry, career)?;
        set(&mut entry, "goal", PV::Message(goal))?;
        let mut attrs = child(&entry.descriptor(), "attributes")?;
        for msg in ["name", "description"] {
            let id = child(&attrs.descriptor(), msg)?;
            set(&mut attrs, msg, PV::Message(id))?;
        }
        set(&mut attrs, "target", PV::U32(1))?;
        set(&mut attrs, "career", PV::EnumNumber(career))?;
        set(&mut entry, "attributes", PV::Message(attrs))?;
        goals.push(PV::Message(entry));
    }
    set(&mut rsp, "goals", PV::List(goals))?;
    Ok(rsp)
}

fn career_goal_states() -> Result<DynamicMessage> {
    let mut rsp = response_for("eve_public.career.goal.api.GetAllRequest")?;
    let mut goals = Vec::new();
    for career in CAREERS {
        let mut entry = child(&rsp.descriptor(), "goals")?;
        let goal = goal_identifier(&entry, career)?;
        set(&mut entry, "goal", PV::Message(goal))?;
        set(&mut entry, "progress", PV::U32(0))?;
        set(&mut entry, "not_completed", PV::Bool(true))?;
        goals.push(PV::Message(entry));
    }
    set(&mut rsp, "goals", PV::List(goals))?;
    Ok(rsp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_and_goals_build() {
        let gw = Gateway { cdn_url: "http://127.0.0.1".into(), ..Default::default() };
        for (req, content) in [
            ("eve_public.achievement.definition.api.cdn.GetLatestRequest", "achievements"),
            ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", "titles"),
            ("eve_public.space.api.cdn.GetUpdateRequest", "maptags"),
        ] {
            let m = checkpoint(&gw, req, content).unwrap();
            assert!(format!("{m}").contains(&format!("http://127.0.0.1/content/{content}")));
        }
        assert_eq!(career_goal_definitions().unwrap().get_field_by_name("goals").unwrap().as_list().unwrap().len(), 4);
        career_goal_states().unwrap();
        stage_thresholds("eve_public.pirate.corruption.api.GetStageThresholdsRequest").unwrap();
    }

    #[test]
    fn defaults_say_absent_and_echo_ids() {
        let caller = Caller { user_id: Some(1), character_id: Some(90000001) };
        let mut m = response_for("eve_public.matrix.api.GetEquippedRequest").unwrap();
        let ids = super::super::known_ids(&m, &caller);
        super::super::fill_defaults(&mut m, 4, &ids);
        assert!(format!("{m}").contains("no_equipped:true"), "{m}");

        let mut m = response_for("eve_public.pirate.corruption.api.GetSystemInfoRequest").unwrap();
        super::super::fill_defaults(&mut m, 4, &Default::default());
        assert!(format!("{m}").contains("denominator:1"), "{m}");

        let mut req = DynamicMessage::new(message("eve_public.cosmetic.ship.api.GetRequest").unwrap());
        let ship = identifier(&req.descriptor(), "ship", PV::U64(1000000000001)).unwrap();
        set(&mut req, "ship", PV::Message(ship)).unwrap();
        let mut m = response_for("eve_public.cosmetic.ship.api.GetRequest").unwrap();
        super::super::fill_defaults(&mut m, 4, &super::super::known_ids(&req, &caller));
        let s = format!("{m}");
        assert!(s.contains("1000000000001") && s.contains("90000001"), "{s}");
    }
}
