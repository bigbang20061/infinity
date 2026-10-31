//! Game policy snapshot, sourced from server defaults and operator TOML.
//! Client-computed rules and SDE data are not configurable here.

use anyhow::Result;
use serde::Deserialize;

#[path = "economy.rs"]
pub mod economy;
#[path = "world.rs"]
pub mod world;
#[path = "social.rs"]
pub mod social;
#[path = "gm.rs"]
pub mod gm;

pub(super) fn duration(key: &str, value: i64, unit: i64, future: bool) -> Result<()> {
    let ticks = value.checked_mul(unit);
    anyhow::ensure!(ticks.is_some_and(|ticks| !future || crate::client::filetime_now().checked_add(ticks).is_some()),
        "{key}={value} exceeds the representable duration or deadline");
    Ok(())
}

fn milliseconds(key: &str, seconds: i64) -> Result<()> {
    let deadline = seconds.checked_mul(crate::space::seconds_to_ms(1.0) as i64)
        .and_then(|interval| crate::space::now_ms().checked_add(interval));
    anyhow::ensure!(deadline.is_some(), "{key}={seconds} exceeds the representable duration or deadline");
    Ok(())
}

/// Immutable gameplay policy captured at startup.
#[derive(Default, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameConfig {
    /// Bounty and kill-right policy.
    pub bounty: super::bounty::BountyConfig,
    /// Accounts policy.
    pub accounts: economy::AccountsConfig,
    /// Characters policy.
    pub characters: economy::CharactersConfig,
    /// Market policy.
    pub market: economy::MarketConfig,
    /// Direct trade policy.
    pub trade: economy::TradeConfig,
    /// Store policy.
    pub store: economy::StoreConfig,
    /// Industry policy.
    pub industry: economy::IndustryConfig,
    /// Missions policy.
    pub missions: world::MissionsConfig,
    /// Standings policy.
    pub standings: world::StandingsConfig,
    /// Combat policy.
    pub combat: world::CombatConfig,
    /// Npc policy.
    pub npc: world::NpcConfig,
    /// Crimewatch policy.
    pub crimewatch: world::CrimewatchConfig,
    /// Mining policy.
    pub mining: world::MiningConfig,
    /// Exploration policy.
    pub exploration: world::ExplorationConfig,
    /// Corporations policy.
    pub corporations: social::CorporationsConfig,
    /// Contracts policy.
    pub contracts: social::ContractsConfig,
    /// Clones policy.
    pub clones: social::ClonesConfig,
    /// Mail policy.
    pub mail: social::MailConfig,
    /// Calendar policy.
    pub calendar: social::CalendarConfig,
    /// Space policy.
    pub space: social::SpaceConfig,
    /// GM spawn policy.
    pub gm: gm::GmConfig,
}

impl GameConfig {
    pub fn validate(&self) -> Result<()> {
        self.bounty.validate()?;
        self.accounts.validate()?;
        self.characters.validate()?;
        self.market.validate()?;
        self.trade.validate()?;
        self.store.validate()?;
        self.industry.validate()?;
        self.missions.validate()?;
        self.standings.validate()?;
        self.combat.validate()?;
        self.npc.validate()?;
        self.crimewatch.validate()?;
        self.mining.validate()?;
        self.exploration.validate()?;
        self.corporations.validate()?;
        self.contracts.validate()?;
        self.clones.validate()?;
        self.mail.validate()?;
        self.calendar.validate()?;
        self.space.validate()?;
        self.gm.validate()?;
        Ok(())
    }
}
