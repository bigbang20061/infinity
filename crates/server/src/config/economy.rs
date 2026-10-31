//! Typed game policy defaults and validation. These values are server policy, not SDE data.
//! They do not change client-computed formulas or migrate gameplay consumers.

use anyhow::Result;
use serde::Deserialize;

/// Largest accepted starting PLEX. Native TOML integer domain is `0..=i64::MAX`.
/// Director server policy: starting PLEX is a native i64 in that range.
const MAX_STARTING_PLEX: i64 = i64::MAX;

/// 15 significant digits (f64 DBL_DIG): the largest two-decimal amount that survives decimal → f64 → PostgreSQL float8::numeric unchanged.
const MAX_STARTING_ISK: f64 = 9_999_999_999_999.99;
/// Daily free offer from services/store/store_manager.rs.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DailyGift {
    /// Gift item type, from the existing free daily offer.
    pub type_id: i64,
    /// Gift quantity, from the existing free daily offer.
    pub quantity: i64,
}
impl Default for DailyGift {
    fn default() -> Self { Self { type_id: 52520, quantity: 10000 } }
}
impl DailyGift {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.type_id > 0, "store.daily_gift.type_id={} must be positive", self.type_id);
        anyhow::ensure!(self.quantity > 0, "store.daily_gift.quantity={} must be positive", self.quantity);
        Ok(())
    }
}

/// Accounts configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccountsConfig {
    /// Default source: game_schema.sql account PLEX vault and services/charsel.rs character-selection policy.
    pub starting_plex: i64,
    /// Default source: game_schema.sql account PLEX vault and services/charsel.rs character-selection policy.
    pub character_slots: i64,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        Self {
            starting_plex: 1000,
            character_slots: 3,
        }
    }
}

impl AccountsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!((0..=MAX_STARTING_PLEX).contains(&self.starting_plex), "accounts.starting_plex={} is outside its valid domain", self.starting_plex);
        anyhow::ensure!(self.character_slots > 0, "accounts.character_slots={} is outside its valid domain", self.character_slots);
        Ok(())
    }
}

/// Characters configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CharactersConfig {
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub starting_isk: f64,
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub name_min_len: i64,
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub name_max_len: i64,
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub name_max_spaces: i64,
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub delete_delay_secs: i64,
    /// Default source: services/charsel.rs and services/skills/skill_handler.rs character policy.
    pub free_respecs: i64,
}

impl Default for CharactersConfig {
    fn default() -> Self {
        Self {
            starting_isk: 5000000.0,
            name_min_len: 3,
            name_max_len: 37,
            name_max_spaces: 2,
            delete_delay_secs: 30,
            free_respecs: 1,
        }
    }
}

impl CharactersConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.starting_isk.is_finite() && self.starting_isk >= 0.0 && self.starting_isk <= MAX_STARTING_ISK, "characters.starting_isk={} is outside its valid domain", self.starting_isk);
        anyhow::ensure!(self.name_min_len > 0, "characters.name_min_len={} is outside its valid domain", self.name_min_len);
        anyhow::ensure!(self.name_max_len > 0, "characters.name_max_len={} is outside its valid domain", self.name_max_len);
        anyhow::ensure!(self.name_max_spaces > 0, "characters.name_max_spaces={} is outside its valid domain", self.name_max_spaces);
        anyhow::ensure!(self.delete_delay_secs > 0, "characters.delete_delay_secs={} is outside its valid domain", self.delete_delay_secs);
        anyhow::ensure!(self.free_respecs > 0, "characters.free_respecs={} is outside its valid domain", self.free_respecs);
        anyhow::ensure!(self.name_min_len <= self.name_max_len, "characters.name_min_len={} exceeds characters.name_max_len={}", self.name_min_len, self.name_max_len);
        Ok(())
    }
}

/// Market configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MarketConfig {
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub broker_fee: f64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub broker_fee_min_isk: f64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub sales_tax: f64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub max_order_days: i64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub npc_buy_ratio: f64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub maintenance_secs: i64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub order_history_days: i64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub transaction_history_days: i64,
    /// Default source: services/market/mod.rs marketutil.const, appConst.mktMinimumFee and server market policy.
    pub price_history_days: i64,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            broker_fee: 0.03,
            broker_fee_min_isk: 100.0,
            sales_tax: 0.08,
            max_order_days: 90,
            npc_buy_ratio: 0.8,
            maintenance_secs: 60,
            order_history_days: 90,
            transaction_history_days: 30,
            price_history_days: 365,
        }
    }
}

impl MarketConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.broker_fee.is_finite() && (0.0..=1.0).contains(&self.broker_fee), "market.broker_fee={} is outside its valid domain", self.broker_fee);
        anyhow::ensure!(self.broker_fee_min_isk.is_finite() && self.broker_fee_min_isk >= 0.0, "market.broker_fee_min_isk={} is outside its valid domain", self.broker_fee_min_isk);
        anyhow::ensure!(self.sales_tax.is_finite() && (0.0..=1.0).contains(&self.sales_tax), "market.sales_tax={} is outside its valid domain", self.sales_tax);
        anyhow::ensure!(self.max_order_days > 0, "market.max_order_days={} is outside its valid domain", self.max_order_days);
        anyhow::ensure!(self.npc_buy_ratio.is_finite() && (0.0..=1.0).contains(&self.npc_buy_ratio), "market.npc_buy_ratio={} is outside its valid domain", self.npc_buy_ratio);
        anyhow::ensure!(self.maintenance_secs > 0, "market.maintenance_secs={} is outside its valid domain", self.maintenance_secs);
        anyhow::ensure!(self.order_history_days > 0, "market.order_history_days={} is outside its valid domain", self.order_history_days);
        anyhow::ensure!(self.transaction_history_days > 0, "market.transaction_history_days={} is outside its valid domain", self.transaction_history_days);
        anyhow::ensure!(self.price_history_days > 0, "market.price_history_days={} is outside its valid domain", self.price_history_days);
        Ok(())
    }
}

/// Store configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Default source: services/store/store_manager.rs and services/store/mod.rs store policy.
    pub plex_price_isk_fallback: f64,
    /// Default source: services/store/store_manager.rs and services/store/mod.rs store policy.
    pub daily_gift: DailyGift,
    /// Default source: services/store/store_manager.rs and services/store/mod.rs store policy.
    pub daily_turnover_utc: String,
    /// Default source: services/store/store_manager.rs and services/store/mod.rs store policy.
    pub fast_checkout: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            plex_price_isk_fallback: 23500000.0,
            daily_gift: DailyGift { type_id: 52520, quantity: 10000 },
            daily_turnover_utc: "03:00:00".to_owned(),
            fast_checkout: false,
        }
    }
}

impl StoreConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.plex_price_isk_fallback.is_finite() && self.plex_price_isk_fallback > 0.0, "store.plex_price_isk_fallback={} is outside its valid domain", self.plex_price_isk_fallback);
        self.daily_gift.validate()?;
        let mut parts = self.daily_turnover_utc.split(':');
        let valid_part = |part: Option<&str>, limit| {
            part.is_some_and(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u8>().is_ok_and(|v| v < limit))
        };
        let valid = valid_part(parts.next(), 24) && valid_part(parts.next(), 60)
            && valid_part(parts.next(), 60) && parts.next().is_none();
        anyhow::ensure!(valid, "store.daily_turnover_utc={:?} must be a UTC time HH:MM:SS", self.daily_turnover_utc);

        Ok(())
    }
}

/// Industry configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndustryConfig {
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub job_cost_rate: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub scc_surcharge_rate: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub highsec_security_tax: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub npc_facility_tax: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub system_cost_index: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub invention_skill_probability: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub job_time_multiplier: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub max_run_secs: f64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub max_runs: i64,
    /// Default source: services/industry/mod.rs industry.const and server policy; job multiplier is a new server policy.
    pub history_days: i64,
}

impl Default for IndustryConfig {
    fn default() -> Self {
        Self {
            job_cost_rate: 0.02,
            scc_surcharge_rate: 0.04,
            highsec_security_tax: 0.05,
            npc_facility_tax: 0.0025,
            system_cost_index: 0.02,
            invention_skill_probability: 0.0333333,
            job_time_multiplier: 1.0,
            max_run_secs: 2592000.0,
            max_runs: 1000000,
            history_days: 90,
        }
    }
}

impl IndustryConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.job_cost_rate.is_finite() && (0.0..=1.0).contains(&self.job_cost_rate), "industry.job_cost_rate={} is outside its valid domain", self.job_cost_rate);
        anyhow::ensure!(self.scc_surcharge_rate.is_finite() && (0.0..=1.0).contains(&self.scc_surcharge_rate), "industry.scc_surcharge_rate={} is outside its valid domain", self.scc_surcharge_rate);
        anyhow::ensure!(self.highsec_security_tax.is_finite() && (0.0..=1.0).contains(&self.highsec_security_tax), "industry.highsec_security_tax={} is outside its valid domain", self.highsec_security_tax);
        anyhow::ensure!(self.npc_facility_tax.is_finite() && (0.0..=1.0).contains(&self.npc_facility_tax), "industry.npc_facility_tax={} is outside its valid domain", self.npc_facility_tax);
        anyhow::ensure!(self.system_cost_index.is_finite() && (0.0..=1.0).contains(&self.system_cost_index), "industry.system_cost_index={} is outside its valid domain", self.system_cost_index);
        anyhow::ensure!(self.invention_skill_probability.is_finite() && (0.0..=1.0).contains(&self.invention_skill_probability), "industry.invention_skill_probability={} is outside its valid domain", self.invention_skill_probability);
        anyhow::ensure!(self.job_time_multiplier.is_finite() && self.job_time_multiplier > 0.0, "industry.job_time_multiplier={} is outside its valid domain", self.job_time_multiplier);
        anyhow::ensure!(self.max_run_secs.is_finite() && self.max_run_secs > 0.0, "industry.max_run_secs={} is outside its valid domain", self.max_run_secs);
        anyhow::ensure!(self.max_runs > 0, "industry.max_runs={} is outside its valid domain", self.max_runs);
        anyhow::ensure!(self.history_days > 0, "industry.history_days={} is outside its valid domain", self.history_days);
        Ok(())
    }
}

