//! Typed game policy defaults and validation. These values are server policy, not SDE data.
//! They do not change client-computed formulas or migrate gameplay consumers.

use anyhow::Result;
use serde::Deserialize;

/// Per-level mission payout from services/missions/mechanics.rs.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MissionReward {
    /// ISK paid for a mission.
    pub isk: i64,
    /// Loyalty points paid for a mission.
    pub lp: i64,
}
impl Default for MissionReward {
    fn default() -> Self { Self { isk: 0, lp: 0 } }
}

/// Missions configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MissionsConfig {
    /// Default source: services/missions/mechanics.rs and services/missions/mod.rs mission policy.
    pub site_distance_m: f64,
    /// Default source: services/missions/mechanics.rs and services/missions/mod.rs mission policy.
    pub spawn_range_m: [i64; 2],
    /// Default source: services/missions/mechanics.rs and services/missions/mod.rs mission policy.
    pub rewards: [MissionReward; 5],
}

impl Default for MissionsConfig {
    fn default() -> Self {
        Self {
            site_distance_m: 15000000.0,
            spawn_range_m: [15000, 40000],
            rewards: [MissionReward { isk: 90000, lp: 150 }, MissionReward { isk: 350000, lp: 450 }, MissionReward { isk: 1000000, lp: 1300 }, MissionReward { isk: 2800000, lp: 3500 }, MissionReward { isk: 7000000, lp: 8000 }],
        }
    }
}

impl MissionsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.site_distance_m.is_finite() && self.site_distance_m > 0.0, "missions.site_distance_m={} is outside its valid domain", self.site_distance_m);
        anyhow::ensure!(self.spawn_range_m.iter().all(|v| *v > 0) && self.spawn_range_m[0] <= self.spawn_range_m[1], "missions.spawn_range_m={:?} must be a positive ordered range", self.spawn_range_m);
        for (i, reward) in self.rewards.iter().enumerate() {
            anyhow::ensure!(reward.isk >= 0, "missions.rewards[{i}].isk={} must be nonnegative", reward.isk);
            anyhow::ensure!(reward.lp >= 0, "missions.rewards[{i}].lp={} must be nonnegative", reward.lp);
        }
        Ok(())
    }
}

/// Standings configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StandingsConfig {
    /// Default source: services/standings mission standing policy.
    pub mission_corp_gain: f64,
    /// Default source: services/standings mission standing policy.
    pub mission_agent_gain_multiplier: f64,
    /// Default source: services/standings mission standing policy.
    pub mission_faction_gain_divisor: f64,
    /// Default source: services/standings mission standing policy.
    pub decline_loss: f64,
}

impl Default for StandingsConfig {
    fn default() -> Self {
        Self {
            mission_corp_gain: 0.01,
            mission_agent_gain_multiplier: 2.0,
            mission_faction_gain_divisor: 5.0,
            decline_loss: -0.01,
        }
    }
}

impl StandingsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.mission_corp_gain.is_finite() && (0.0..=1.0).contains(&self.mission_corp_gain), "standings.mission_corp_gain={} is outside its valid domain", self.mission_corp_gain);
        anyhow::ensure!(self.mission_agent_gain_multiplier.is_finite() && self.mission_agent_gain_multiplier > 0.0, "standings.mission_agent_gain_multiplier={} is outside its valid domain", self.mission_agent_gain_multiplier);
        anyhow::ensure!(self.mission_faction_gain_divisor.is_finite() && self.mission_faction_gain_divisor > 0.0, "standings.mission_faction_gain_divisor={} is outside its valid domain", self.mission_faction_gain_divisor);
        anyhow::ensure!(self.decline_loss.is_finite() && (-10.0..=0.0).contains(&self.decline_loss), "standings.decline_loss={} is outside its valid domain", self.decline_loss);
        Ok(())
    }
}

/// Combat configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CombatConfig {
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub bounty_multiplier: f64,
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub lock_time_factor: f64,
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub lock_time_min_secs: f64,
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub lock_time_max_secs: f64,
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub npc_think_ms: i64,
    /// Default source: space/mod.rs and space/npc.rs targeting and NPC policy; bounty multiplier is a new server policy.
    pub npc_target_loss_range_factor: f64,
}

impl Default for CombatConfig {
    fn default() -> Self {
        Self {
            bounty_multiplier: 1.0,
            lock_time_factor: 40000.0,
            lock_time_min_secs: 0.5,
            lock_time_max_secs: 60.0,
            npc_think_ms: 1000,
            npc_target_loss_range_factor: 2.0,
        }
    }
}

impl CombatConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.bounty_multiplier.is_finite() && self.bounty_multiplier >= 0.0, "combat.bounty_multiplier={} is outside its valid domain", self.bounty_multiplier);
        anyhow::ensure!(self.lock_time_factor.is_finite() && self.lock_time_factor > 0.0, "combat.lock_time_factor={} is outside its valid domain", self.lock_time_factor);
        anyhow::ensure!(self.lock_time_min_secs.is_finite() && self.lock_time_min_secs > 0.0, "combat.lock_time_min_secs={} is outside its valid domain", self.lock_time_min_secs);
        anyhow::ensure!(self.lock_time_max_secs.is_finite() && self.lock_time_max_secs > 0.0, "combat.lock_time_max_secs={} is outside its valid domain", self.lock_time_max_secs);
        anyhow::ensure!(self.npc_think_ms > 0, "combat.npc_think_ms={} is outside its valid domain", self.npc_think_ms);
        anyhow::ensure!(self.npc_target_loss_range_factor.is_finite() && self.npc_target_loss_range_factor > 0.0, "combat.npc_target_loss_range_factor={} is outside its valid domain", self.npc_target_loss_range_factor);
        anyhow::ensure!(self.lock_time_min_secs <= self.lock_time_max_secs, "combat.lock_time_min_secs={} exceeds combat.lock_time_max_secs={}", self.lock_time_min_secs, self.lock_time_max_secs);
        Ok(())
    }
}

/// Npc configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NpcConfig {
    /// Default source: services/exploration site and belt spawn policy.
    pub belt_rats: bool,
    /// Default source: services/exploration site and belt spawn policy.
    pub belt_rat_respawn_secs: i64,
    /// Default source: services/exploration site and belt spawn policy.
    pub belt_spawn_range_m: [i64; 2],
    /// Default source: services/exploration site and belt spawn policy.
    pub site_spawn_range_m: [i64; 2],
    /// Default source: services/exploration site and belt spawn policy.
    pub cleared_site_linger_secs: i64,
    /// Default source: services/exploration site and belt spawn policy.
    pub hacked_site_linger_secs: i64,
}

impl Default for NpcConfig {
    fn default() -> Self {
        Self {
            belt_rats: true,
            belt_rat_respawn_secs: 900,
            belt_spawn_range_m: [5000, 20000],
            site_spawn_range_m: [15000, 40000],
            cleared_site_linger_secs: 30,
            hacked_site_linger_secs: 600,
        }
    }
}

impl NpcConfig {
    pub fn validate(&self) -> Result<()> {

        anyhow::ensure!(self.belt_rat_respawn_secs > 0, "npc.belt_rat_respawn_secs={} is outside its valid domain", self.belt_rat_respawn_secs);
        anyhow::ensure!(self.belt_spawn_range_m.iter().all(|v| *v > 0) && self.belt_spawn_range_m[0] <= self.belt_spawn_range_m[1], "npc.belt_spawn_range_m={:?} must be a positive ordered range", self.belt_spawn_range_m);
        anyhow::ensure!(self.site_spawn_range_m.iter().all(|v| *v > 0) && self.site_spawn_range_m[0] <= self.site_spawn_range_m[1], "npc.site_spawn_range_m={:?} must be a positive ordered range", self.site_spawn_range_m);
        anyhow::ensure!(self.cleared_site_linger_secs > 0, "npc.cleared_site_linger_secs={} is outside its valid domain", self.cleared_site_linger_secs);
        anyhow::ensure!(self.hacked_site_linger_secs > 0, "npc.hacked_site_linger_secs={} is outside its valid domain", self.hacked_site_linger_secs);
        Ok(())
    }
}

/// Crimewatch configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CrimewatchConfig {
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub concord_enabled: bool,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub weapons_timer_secs: i64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub pvp_timer_secs: i64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub criminal_timer_secs: i64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub engagement_secs: i64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub outlaw_threshold: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub highsec_threshold: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub concord_base_delay_secs: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub concord_delay_per_security_secs: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub security_gain_npc_kill: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub security_loss_aggression: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub security_loss_kill: f64,
    /// Default source: services/crimewatch/mod.rs Crimewatch policy; CONCORD toggle is a new server policy.
    pub killmail_attacker_window_secs: i64,
}

impl Default for CrimewatchConfig {
    fn default() -> Self {
        Self {
            concord_enabled: true,
            weapons_timer_secs: 60,
            pvp_timer_secs: 900,
            criminal_timer_secs: 900,
            engagement_secs: 300,
            outlaw_threshold: -5.0,
            highsec_threshold: 0.45,
            concord_base_delay_secs: 6.0,
            concord_delay_per_security_secs: 28.0,
            security_gain_npc_kill: 0.0005,
            security_loss_aggression: 0.025,
            security_loss_kill: 0.05,
            killmail_attacker_window_secs: 300,
        }
    }
}

impl CrimewatchConfig {
    pub fn validate(&self) -> Result<()> {

        anyhow::ensure!(self.weapons_timer_secs > 0, "crimewatch.weapons_timer_secs={} is outside its valid domain", self.weapons_timer_secs);
        anyhow::ensure!(self.pvp_timer_secs > 0, "crimewatch.pvp_timer_secs={} is outside its valid domain", self.pvp_timer_secs);
        anyhow::ensure!(self.criminal_timer_secs > 0, "crimewatch.criminal_timer_secs={} is outside its valid domain", self.criminal_timer_secs);
        anyhow::ensure!(self.engagement_secs > 0, "crimewatch.engagement_secs={} is outside its valid domain", self.engagement_secs);
        anyhow::ensure!(self.outlaw_threshold.is_finite() && (-10.0..=10.0).contains(&self.outlaw_threshold), "crimewatch.outlaw_threshold={} is outside its valid domain", self.outlaw_threshold);
        anyhow::ensure!(self.highsec_threshold.is_finite() && (0.0..=1.0).contains(&self.highsec_threshold), "crimewatch.highsec_threshold={} is outside its valid domain", self.highsec_threshold);
        anyhow::ensure!(self.concord_base_delay_secs.is_finite() && self.concord_base_delay_secs > 0.0, "crimewatch.concord_base_delay_secs={} is outside its valid domain", self.concord_base_delay_secs);
        anyhow::ensure!(self.concord_delay_per_security_secs.is_finite() && self.concord_delay_per_security_secs > 0.0, "crimewatch.concord_delay_per_security_secs={} is outside its valid domain", self.concord_delay_per_security_secs);
        anyhow::ensure!(self.security_gain_npc_kill.is_finite() && (0.0..=1.0).contains(&self.security_gain_npc_kill), "crimewatch.security_gain_npc_kill={} is outside its valid domain", self.security_gain_npc_kill);
        anyhow::ensure!(self.security_loss_aggression.is_finite() && (0.0..=1.0).contains(&self.security_loss_aggression), "crimewatch.security_loss_aggression={} is outside its valid domain", self.security_loss_aggression);
        anyhow::ensure!(self.security_loss_kill.is_finite() && (0.0..=1.0).contains(&self.security_loss_kill), "crimewatch.security_loss_kill={} is outside its valid domain", self.security_loss_kill);
        anyhow::ensure!(self.killmail_attacker_window_secs > 0, "crimewatch.killmail_attacker_window_secs={} is outside its valid domain", self.killmail_attacker_window_secs);
        Ok(())
    }
}

/// Mining configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MiningConfig {
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub belt_regrowth_secs: i64,
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub ore_rocks: [i64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub ore_rock_m3: [f64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub ice_rocks: [i64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub ice_rock_units: [i64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub gas_clouds: [i64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub gas_cloud_m3: [f64; 2],
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub station_reprocessing_efficiency: f64,
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub station_reprocessing_tax: f64,
    /// Default source: services/mining.rs mining and reprocessing policy.
    pub scanner_range_m: f64,
}

impl Default for MiningConfig {
    fn default() -> Self {
        Self {
            belt_regrowth_secs: 7200,
            ore_rocks: [15, 25],
            ore_rock_m3: [4000.0, 25000.0],
            ice_rocks: [6, 11],
            ice_rock_units: [300, 1500],
            gas_clouds: [5, 9],
            gas_cloud_m3: [3000.0, 15000.0],
            station_reprocessing_efficiency: 0.5,
            station_reprocessing_tax: 0.05,
            scanner_range_m: 500000.0,
        }
    }
}

impl MiningConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.belt_regrowth_secs > 0, "mining.belt_regrowth_secs={} is outside its valid domain", self.belt_regrowth_secs);
        anyhow::ensure!(self.ore_rocks.iter().all(|v| *v > 0) && self.ore_rocks[0] <= self.ore_rocks[1], "mining.ore_rocks={:?} must be a positive ordered range", self.ore_rocks);
        anyhow::ensure!(self.ore_rock_m3.iter().all(|v| v.is_finite() && *v > 0.0) && self.ore_rock_m3[0] <= self.ore_rock_m3[1], "mining.ore_rock_m3={:?} must be a positive ordered range", self.ore_rock_m3);
        anyhow::ensure!(self.ice_rocks.iter().all(|v| *v > 0) && self.ice_rocks[0] <= self.ice_rocks[1], "mining.ice_rocks={:?} must be a positive ordered range", self.ice_rocks);
        anyhow::ensure!(self.ice_rock_units.iter().all(|v| *v > 0) && self.ice_rock_units[0] <= self.ice_rock_units[1], "mining.ice_rock_units={:?} must be a positive ordered range", self.ice_rock_units);
        anyhow::ensure!(self.gas_clouds.iter().all(|v| *v > 0) && self.gas_clouds[0] <= self.gas_clouds[1], "mining.gas_clouds={:?} must be a positive ordered range", self.gas_clouds);
        anyhow::ensure!(self.gas_cloud_m3.iter().all(|v| v.is_finite() && *v > 0.0) && self.gas_cloud_m3[0] <= self.gas_cloud_m3[1], "mining.gas_cloud_m3={:?} must be a positive ordered range", self.gas_cloud_m3);
        anyhow::ensure!(self.station_reprocessing_efficiency.is_finite() && (0.0..=1.0).contains(&self.station_reprocessing_efficiency), "mining.station_reprocessing_efficiency={} is outside its valid domain", self.station_reprocessing_efficiency);
        anyhow::ensure!(self.station_reprocessing_tax.is_finite() && (0.0..=1.0).contains(&self.station_reprocessing_tax), "mining.station_reprocessing_tax={} is outside its valid domain", self.station_reprocessing_tax);
        anyhow::ensure!(self.scanner_range_m.is_finite() && self.scanner_range_m > 0.0, "mining.scanner_range_m={} is outside its valid domain", self.scanner_range_m);
        Ok(())
    }
}

/// Exploration configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExplorationConfig {
    /// Default source: services/exploration generation and scanning policy.
    pub ice_site_chance: f64,
    /// Default source: services/exploration generation and scanning policy.
    pub gas_site_chance: f64,
    /// Default source: services/exploration generation and scanning policy.
    pub scan_uncertainty_floor_m: f64,
    /// Default source: services/exploration generation and scanning policy.
    pub wormhole_lifetime_min: i64,
    /// Default source: services/exploration generation and scanning policy.
    pub wormhole_total_mass_kg: i64,
    /// Default source: services/exploration generation and scanning policy.
    pub wormhole_jump_mass_kg: i64,
}

impl Default for ExplorationConfig {
    fn default() -> Self {
        Self {
            ice_site_chance: 0.3,
            gas_site_chance: 0.5,
            scan_uncertainty_floor_m: 10000000.0,
            wormhole_lifetime_min: 960,
            wormhole_total_mass_kg: 1000000000,
            wormhole_jump_mass_kg: 300000000,
        }
    }
}

impl ExplorationConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.ice_site_chance.is_finite() && (0.0..=1.0).contains(&self.ice_site_chance), "exploration.ice_site_chance={} is outside its valid domain", self.ice_site_chance);
        anyhow::ensure!(self.gas_site_chance.is_finite() && (0.0..=1.0).contains(&self.gas_site_chance), "exploration.gas_site_chance={} is outside its valid domain", self.gas_site_chance);
        anyhow::ensure!(self.scan_uncertainty_floor_m.is_finite() && self.scan_uncertainty_floor_m > 0.0, "exploration.scan_uncertainty_floor_m={} is outside its valid domain", self.scan_uncertainty_floor_m);
        anyhow::ensure!(self.wormhole_lifetime_min > 0, "exploration.wormhole_lifetime_min={} is outside its valid domain", self.wormhole_lifetime_min);
        anyhow::ensure!(self.wormhole_total_mass_kg > 0, "exploration.wormhole_total_mass_kg={} is outside its valid domain", self.wormhole_total_mass_kg);
        anyhow::ensure!(self.wormhole_jump_mass_kg > 0, "exploration.wormhole_jump_mass_kg={} is outside its valid domain", self.wormhole_jump_mass_kg);
        anyhow::ensure!(self.wormhole_jump_mass_kg <= self.wormhole_total_mass_kg, "exploration.wormhole_jump_mass_kg={} exceeds exploration.wormhole_total_mass_kg={}", self.wormhole_jump_mass_kg, self.wormhole_total_mass_kg);
        Ok(())
    }
}

