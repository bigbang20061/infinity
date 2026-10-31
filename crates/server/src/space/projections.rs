//! Actor-owned, transient incoming dogma projections and target refreshes.
//! Client dogma consumes OnEwarStart/End for projected effect kinds; SDE effect
//! rows supply notification classification and modifiers. Projections do not survive system shutdown.
use super::*;
use crate::services::{effects::{notify, projection::Projection, propulsion, registry, EffectKind}, fit::Fit};
use crate::consts::{ATTR_MAX_TARGET_RANGE, ATTR_SCAN_RESOLUTION};
use crate::services::fit::Key;

impl System {
    /// Snapshot the current incoming sources for one target.
    pub(crate) fn projections_on(&self, target: i64) -> Vec<Projection> {
        self.projections.get(&target).cloned().unwrap_or_default()
    }

    /// Generation of the target's current incoming projection snapshot.
    pub(crate) fn projection_revision(&self, target: i64) -> u64 {
        self.projection_revisions.get(&target).copied().unwrap_or(0)
    }

    /// Replace the snapshot for one source module, moving it between targets if needed.
    pub(crate) fn upsert_projection(&mut self, target: i64, projection: Projection) {
        if !self.contains(target) || !self.contains(projection.source_ship) { return; }
        let previous = self.take_projection(projection.source_module);
        let changed_identity = previous.as_ref().is_none_or(|(old_target, old)|
            *old_target != target || old.effect_id != projection.effect_id || old.source_ship != projection.source_ship || old.kind != projection.kind);
        if let Some((old_target, old)) = previous {
            if changed_identity {
                self.projection_notice(old_target, &old, false);
                if old_target != target { self.refresh_projection_target(old_target); }
            }
        }
        if changed_identity { self.projection_notice(target, &projection, true); }
        self.projection_targets.insert(projection.source_module, target);
        let entries = self.projections.entry(target).or_default();
        let index = entries.binary_search_by_key(&projection.source_module, |p| p.source_module).unwrap_or_else(|i| i);
        entries.insert(index, projection);
        self.refresh_projection_target(target);
    }

    fn take_projection(&mut self, source_module: i64) -> Option<(i64, Projection)> {
        let target = self.projection_targets.remove(&source_module)?;
        let entries = self.projections.get_mut(&target).expect("projection target index");
        let index = entries.binary_search_by_key(&source_module, |p| p.source_module).expect("projection module index");
        let projection = entries.remove(index);
        if entries.is_empty() { self.projections.remove(&target); }
        Some((target, projection))
    }

    /// Remove a module's projection, if present.
    pub(crate) fn remove_projection(&mut self, source_module: i64) {
        if let Some((target, projection)) = self.take_projection(source_module) {
            self.projection_notice(target, &projection, false);
            self.refresh_projection_target(target);
        }
    }

    /// Remove outgoing projections of a source against a lost target.
    pub(crate) fn remove_projections_between(&mut self, source_ship: i64, target: i64) {
        let modules: Vec<_> = self.projections.get(&target).into_iter().flatten()
            .filter_map(|p| (p.source_ship == source_ship).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    /// Remove all outgoing projections of a departing source.
    pub(crate) fn remove_projections_from(&mut self, source_ship: i64) {
        let modules: Vec<_> = self.projections.values().flatten()
            .filter_map(|p| (p.source_ship == source_ship).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    pub(super) fn remove_projections_on_departure(&mut self, target: i64) {
        let modules: Vec<_> = self.projections.get(&target).into_iter().flatten().map(|p| p.source_module).collect();
        for id in modules { self.remove_projection(id); }
        let revision = self.projection_revisions.entry(target).or_default();
        *revision = revision.checked_add(1).expect("projection revision exhausted");
    }

    pub(super) fn expire_projections(&mut self) {
        let modules: Vec<_> = self.projections.values().flatten()
            .filter_map(|p| (p.until_ms <= self.ms).then_some(p.source_module)).collect();
        for id in modules { self.remove_projection(id); }
    }

    fn projection_notice(&self, target: i64, projection: &Projection, start: bool) {
        if !matches!(projection.kind, EffectKind::Projected { .. }) { return; }
        let Some(character) = self.ball(target).and_then(|ball| ball.char_id) else { return };
        let Some(effect) = self.projection_sde.as_ref().and_then(|sde| registry::resolve(sde, projection.effect_id)) else { return };
        if let Some(slot) = notify::ewar_slot(&effect) {
            let (event, args) = notify::ewar(projection.source_ship, projection.source_module, target, slot, start);
            self.notify_pilot(character, event, args);
        }
    }

    pub(crate) fn refresh_projection_target(&mut self, target: i64) {
        let revision = self.projection_revisions.entry(target).or_default();
        *revision = revision.checked_add(1).expect("projection revision exhausted");
        let revision = *revision;
        let Some(ball) = self.ball(target) else { return };
        let character = ball.char_id;
        let type_id = ball.type_id;
        let projections = self.projections_on(target);
        let Some(sde) = self.projection_sde.clone() else { return };
        let Some(context) = self.snapshot_context.clone() else { return };
        let has_target_range = sde.types.attribute(type_id, ATTR_MAX_TARGET_RANGE).is_some()
            || sde.dogma.attribute(ATTR_MAX_TARGET_RANGE).and_then(|a| a.default_value).is_some_and(|value| value > 0.0);
        self.defer(async move {
            let fit = if let Some(character) = character {
                match Fit::load_projected(&context.db, sde, character, target, projections).await {
                    Ok(fit) => fit,
                    Err(crate::services::CallError::Internal(error)) => { tracing::warn!(%error, target, "projected fit refresh failed"); return; }
                    Err(crate::services::CallError::User { msg, .. }) => { tracing::warn!(%msg, target, "projected fit refresh failed"); return; }
                }
            } else { Fit::for_space_type(sde, type_id, projections) };
            let values = propulsion::kinematics(&fit);
            let targeting = (fit.value(Key::Ship, ATTR_SCAN_RESOLUTION),
                has_target_range.then(|| fit.value(Key::Ship, ATTR_MAX_TARGET_RANGE)));
            if let Some(tx) = context.tx.upgrade() {
                let _ = tx.send(Box::new(move |system| {
                    if system.contains(target) && system.projection_revisions.get(&target) == Some(&revision) {
                        system.queue_projection_kinematics(target, revision, values);
                        system.refresh_npc_targeting(target, targeting.0, targeting.1);
                    }
                }));
            }
        });
    }
}
