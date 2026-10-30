//! Immutable map records loaded from typed SDE map and station tables at startup.
//! The dataset has no direct client mirror; consumers include
//! `eve/client/script/remote/michelle.py:491-532` and
//! `eve/client/script/parklife/autopilot.py:260-261,329-332`.
//! Map presentation and other SDE data are outside this dataset.

use std::collections::HashMap;
use crate::{dao::sde::map as dao, db::Db};

pub(crate) struct SolarSystem {
    pub solar_system_id: i64,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub name: Option<String>,
    pub constellation_id: Option<i64>,
    pub region_id: Option<i64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub faction_id: Option<i64>,
    pub security: Option<f64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub x: Option<f64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub y: Option<f64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub z: Option<f64>,
}
pub(crate) struct Region {
    pub region_id: i64,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub wormhole_class_id: Option<i64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub nebula_id: Option<i64>,
}
pub(crate) struct Constellation {
    pub constellation_id: i64,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub wormhole_class_id: Option<i64>,
}
pub(crate) struct Stargate {
    pub stargate_id: i64,
    pub solar_system_id: Option<i64>,
    pub dest_stargate_id: Option<i64>,
    pub dest_solar_system_id: Option<i64>,
}
pub(crate) struct Station {
    pub station_id: i64,
    pub type_id: Option<i64>,
    pub owner_id: Option<i64>,
    pub solar_system_id: Option<i64>,
    pub constellation_id: Option<i64>,
    pub region_id: Option<i64>,
    pub operation_id: Option<i64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub x: Option<f64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub y: Option<f64>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub z: Option<f64>,
    pub security: Option<f64>,
}
pub(crate) struct Celestial {
    pub item_id: i64,
    pub type_id: Option<i64>,
    pub solar_system_id: Option<i64>,
    pub group_id: Option<i64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub radius: Option<f64>,
}

pub(crate) struct Map {
    systems: Vec<SolarSystem>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    regions: Vec<Region>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    constellations: Vec<Constellation>,
    stargates: Vec<Stargate>,
    stations: Vec<Station>,
    celestials: Vec<Celestial>,
    system_index: HashMap<i64, usize>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    region_index: HashMap<i64, usize>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    constellation_index: HashMap<i64, usize>,
    stargate_index: HashMap<i64, usize>,
    station_index: HashMap<i64, usize>,
    celestial_index: HashMap<i64, usize>,
    system_celestials: HashMap<i64, Vec<usize>>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    system_stations: HashMap<i64, Vec<usize>>,
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    owner_stations: HashMap<i64, Vec<usize>>,
    jumps: HashMap<i64, Vec<i64>>,
}

impl Map {
    pub(crate) async fn load(db: &Db) -> anyhow::Result<Self> {
        let (systems, regions, constellations, stargates, stations, celestials, jumps) = tokio::try_join!(
            dao::systems(db), dao::regions(db), dao::constellations(db), dao::stargates(db),
            dao::stations(db), dao::celestials(db), dao::jumps(db),
        )?;
        let system_index = systems.iter().enumerate().map(|(i, r)| (r.solar_system_id, i)).collect();
        let region_index = regions.iter().enumerate().map(|(i, r)| (r.region_id, i)).collect();
        let constellation_index = constellations.iter().enumerate().map(|(i, r)| (r.constellation_id, i)).collect();
        let stargate_index = stargates.iter().enumerate().map(|(i, r)| (r.stargate_id, i)).collect();
        let station_index = stations.iter().enumerate().map(|(i, r)| (r.station_id, i)).collect();
        let celestial_index = celestials.iter().enumerate().map(|(i, r)| (r.item_id, i)).collect();
        let mut system_celestials: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, row) in celestials.iter().enumerate() {
            if let Some(system) = row.solar_system_id { system_celestials.entry(system).or_default().push(i); }
        }
        let mut system_stations: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut owner_stations: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, row) in stations.iter().enumerate() {
            if let Some(system) = row.solar_system_id { system_stations.entry(system).or_default().push(i); }
            if let Some(owner) = row.owner_id { owner_stations.entry(owner).or_default().push(i); }
        }
        let mut adjacency: HashMap<i64, Vec<i64>> = HashMap::new();
        for row in jumps { adjacency.entry(row.from_solar_system_id).or_default().push(row.to_solar_system_id); }
        for destinations in adjacency.values_mut() { destinations.sort_unstable(); destinations.dedup(); }
        Ok(Self { systems, regions, constellations, stargates, stations, celestials, system_index, region_index, constellation_index, stargate_index, station_index, celestial_index, system_celestials, system_stations, owner_stations, jumps: adjacency })
    }
    pub(crate) fn system(&self, id: i64) -> Option<&SolarSystem> { self.system_index.get(&id).map(|&i| &self.systems[i]) }
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub(crate) fn region(&self, id: i64) -> Option<&Region> { self.region_index.get(&id).map(|&i| &self.regions[i]) }
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub(crate) fn constellation(&self, id: i64) -> Option<&Constellation> { self.constellation_index.get(&id).map(|&i| &self.constellations[i]) }
    pub(crate) fn stargate(&self, id: i64) -> Option<&Stargate> { self.stargate_index.get(&id).map(|&i| &self.stargates[i]) }
    pub(crate) fn station(&self, id: i64) -> Option<&Station> { self.station_index.get(&id).map(|&i| &self.stations[i]) }
    pub(crate) fn celestial(&self, id: i64) -> Option<&Celestial> { self.celestial_index.get(&id).map(|&i| &self.celestials[i]) }
    pub(crate) fn systems(&self) -> impl Iterator<Item = &SolarSystem> { self.systems.iter() }
    pub(crate) fn stations(&self) -> impl Iterator<Item = &Station> { self.stations.iter() }
    pub(crate) fn all_celestials(&self) -> impl Iterator<Item = &Celestial> { self.celestials.iter() }
    pub(crate) fn celestials(&self, system: i64) -> impl Iterator<Item = &Celestial> { self.system_celestials.get(&system).into_iter().flatten().map(|&i| &self.celestials[i]) }
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub(crate) fn stations_in_system(&self, system: i64) -> impl Iterator<Item = &Station> { self.system_stations.get(&system).into_iter().flatten().map(|&i| &self.stations[i]) }
    #[expect(dead_code, reason = "read by Phase 1 of #5")]
    pub(crate) fn stations_by_owner(&self, owner: i64) -> impl Iterator<Item = &Station> { self.owner_stations.get(&owner).into_iter().flatten().map(|&i| &self.stations[i]) }
    pub(crate) fn jumps(&self, system: i64) -> &[i64] { self.jumps.get(&system).map_or(&[], Vec::as_slice) }
}
