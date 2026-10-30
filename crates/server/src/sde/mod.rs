//! Immutable server-startup datasets for inventory, dogma, maps, factions, corporations, localization, missions, dungeons, industry and store.
//! The loader has no direct client mirror; consumers include `dogma/effects/modifiers.py:8-140`,
//! `dogma/const.py:7-51`, `eve/common/script/dogma/baseDogmaLocation.py:1960-1967`,
//! `eve/client/script/remote/michelle.py:491-532` and
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`.
//! Sources are typed SDE and selected native projections via `dao::sde`.
//! Phase 1 typed races, bloodlines and npcchars are not loaded until their group is migrated.
//! Typed dogma_effect_modifiers and build_info are intentionally unused: modifiers come from dogma_effects instead.
//! Native blueprints and stationoperations are unused under the typed-table ruling.
pub(crate) mod corporations;
pub(crate) mod dogma;
pub(crate) mod dungeons;
pub(crate) mod factions;
pub(crate) mod industry;
pub(crate) mod localization;
pub(crate) mod map;
pub(crate) mod missions;
pub(crate) mod store;
pub(crate) mod types;
pub(crate) mod wrecks;

use crate::db::Db;
use std::time::Instant;

pub(crate) struct Sde {
    pub(crate) types: types::Types,
    pub(crate) dogma: dogma::Dogma,
    pub(crate) map: map::Map,
    pub(crate) missions: missions::Missions,
    pub(crate) dungeons: dungeons::Dungeons,
    pub(crate) factions: factions::Factions,
    pub(crate) npc_corporations: corporations::NpcCorporations,
    pub(crate) localization: localization::Localization,
    pub(crate) wrecks: wrecks::Wrecks,
    pub(crate) industry: industry::Industry,
    pub(crate) store: store::Store,
}
impl Sde {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let started = Instant::now();
        let (mut types, dogma, map, factions, npc_corporations, localization, wrecks, industry, store, missions, mut dungeons) = tokio::try_join!(
            types::Types::load(db), dogma::Dogma::load(db), map::Map::load(db),
            factions::Factions::load(db), corporations::NpcCorporations::load(db), localization::Localization::load(db), wrecks::Wrecks::load(db),
            async { industry::Industry::load(db).await.map_err(anyhow::Error::from) },
            async { store::Store::load(db).await.map_err(anyhow::Error::from) },
            missions::Missions::load(db), dungeons::Dungeons::load(db),
        )?;
        types.index_modifiers(&dogma);
        dungeons.index_map(&map);
        tracing::info!(types = types.len(), dogma_attributes = dogma.attribute_count(), dogma_effects = dogma.effect_count(), systems = map.systems().count(), stations = map.stations().count(), celestials = map.all_celestials().count(), factions = factions.len(), npc_corporations = npc_corporations.len(), localization = localization.len(), elapsed_ms = started.elapsed().as_millis(), "SDE loaded");
        Ok(Self { types, dogma, map, factions, npc_corporations, localization, wrecks, industry, store, missions, dungeons })
    }
}
