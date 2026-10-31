//! Typed game policy defaults and validation. These values are server policy, not SDE data.
//! They do not change client-computed formulas or migrate gameplay consumers.

use anyhow::Result;
use serde::Deserialize;

/// Corporations configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorporationsConfig {
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub min_management_skill_level: i64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub initial_shares: i64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub max_bulletins: i64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub office_slots_per_station: i64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub office_rent_base_isk: f64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub office_rent_per_security_isk: f64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub bills_maintenance_secs: i64,
    /// Default source: services/corps, services/offices.rs, services/bills.rs and services/orgs server policy.
    pub advance_billing_days: i64,
}

impl Default for CorporationsConfig {
    fn default() -> Self {
        Self {
            min_management_skill_level: 1,
            initial_shares: 1000,
            max_bulletins: 10,
            office_slots_per_station: 24,
            office_rent_base_isk: 100000.0,
            office_rent_per_security_isk: 900000.0,
            bills_maintenance_secs: 600,
            advance_billing_days: 7,
        }
    }
}

impl CorporationsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!((1..=5).contains(&self.min_management_skill_level), "corporations.min_management_skill_level={} is outside its valid domain", self.min_management_skill_level);
        anyhow::ensure!(self.initial_shares > 0, "corporations.initial_shares={} is outside its valid domain", self.initial_shares);
        anyhow::ensure!(self.max_bulletins > 0, "corporations.max_bulletins={} is outside its valid domain", self.max_bulletins);
        anyhow::ensure!(self.office_slots_per_station > 0, "corporations.office_slots_per_station={} is outside its valid domain", self.office_slots_per_station);
        anyhow::ensure!(self.office_rent_base_isk.is_finite() && self.office_rent_base_isk >= 0.0, "corporations.office_rent_base_isk={} is outside its valid domain", self.office_rent_base_isk);
        anyhow::ensure!(self.office_rent_per_security_isk.is_finite() && self.office_rent_per_security_isk >= 0.0, "corporations.office_rent_per_security_isk={} is outside its valid domain", self.office_rent_per_security_isk);
        anyhow::ensure!(self.bills_maintenance_secs > 0, "corporations.bills_maintenance_secs={} is outside its valid domain", self.bills_maintenance_secs);
        anyhow::ensure!(self.advance_billing_days > 0, "corporations.advance_billing_days={} is outside its valid domain", self.advance_billing_days);
        Ok(())
    }
}

/// Contracts configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContractsConfig {
    /// Default source: services/contracts lifecycle and pagination policy.
    pub expiry_min_minutes: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub expiry_max_days: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub courier_min_days: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub courier_max_days: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub max_items: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub title_max_len: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub description_max_len: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub page_size: i64,
    /// Default source: services/contracts lifecycle and pagination policy.
    pub page_size_max: i64,
}

impl Default for ContractsConfig {
    fn default() -> Self {
        Self {
            expiry_min_minutes: 60,
            expiry_max_days: 90,
            courier_min_days: 1,
            courier_max_days: 365,
            max_items: 500,
            title_max_len: 100,
            description_max_len: 4000,
            page_size: 100,
            page_size_max: 1000,
        }
    }
}

impl ContractsConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.expiry_min_minutes > 0, "contracts.expiry_min_minutes={} is outside its valid domain", self.expiry_min_minutes);
        anyhow::ensure!(self.expiry_max_days > 0, "contracts.expiry_max_days={} is outside its valid domain", self.expiry_max_days);
        anyhow::ensure!(self.courier_min_days > 0, "contracts.courier_min_days={} is outside its valid domain", self.courier_min_days);
        anyhow::ensure!(self.courier_max_days > 0, "contracts.courier_max_days={} is outside its valid domain", self.courier_max_days);
        anyhow::ensure!(self.max_items > 0, "contracts.max_items={} is outside its valid domain", self.max_items);
        anyhow::ensure!(self.title_max_len > 0, "contracts.title_max_len={} is outside its valid domain", self.title_max_len);
        anyhow::ensure!(self.description_max_len > 0, "contracts.description_max_len={} is outside its valid domain", self.description_max_len);
        anyhow::ensure!(self.page_size > 0, "contracts.page_size={} is outside its valid domain", self.page_size);
        anyhow::ensure!(self.page_size_max > 0, "contracts.page_size_max={} is outside its valid domain", self.page_size_max);
        anyhow::ensure!(self.courier_min_days <= self.courier_max_days, "contracts.courier_min_days={} exceeds contracts.courier_max_days={}", self.courier_min_days, self.courier_max_days);
        anyhow::ensure!(self.page_size <= self.page_size_max, "contracts.page_size={} exceeds contracts.page_size_max={}", self.page_size, self.page_size_max);
        anyhow::ensure!(i128::from(self.expiry_min_minutes) <= i128::from(self.expiry_max_days) * 1440, "contracts.expiry_min_minutes={} exceeds contracts.expiry_max_days={}", self.expiry_min_minutes, self.expiry_max_days);
        Ok(())
    }
}

/// Clones configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClonesConfig {
    /// Default source: services/clones.rs jump clone policy.
    pub jump_cooldown_hours: i64,
    /// Default source: services/clones.rs jump clone policy.
    pub synchronizing_hours_per_level: i64,
}

impl Default for ClonesConfig {
    fn default() -> Self {
        Self {
            jump_cooldown_hours: 24,
            synchronizing_hours_per_level: 1,
        }
    }
}

impl ClonesConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.jump_cooldown_hours > 0, "clones.jump_cooldown_hours={} is outside its valid domain", self.jump_cooldown_hours);
        anyhow::ensure!(self.synchronizing_hours_per_level > 0, "clones.synchronizing_hours_per_level={} is outside its valid domain", self.synchronizing_hours_per_level);
        Ok(())
    }
}

/// Mail configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailConfig {
    /// Default source: services/mail/mail_mgr.rs server mail policy.
    pub title_max_len: i64,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            title_max_len: 150,
        }
    }
}

impl MailConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.title_max_len > 0, "mail.title_max_len={} is outside its valid domain", self.title_max_len);
        Ok(())
    }
}

/// Calendar configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CalendarConfig {
    /// Default source: services/calendar/mod.rs calendar policy.
    pub max_invitees: i64,
    /// Default source: services/calendar/mod.rs calendar policy.
    pub title_max_len: i64,
    /// Default source: services/calendar/mod.rs calendar policy.
    pub description_max_len: i64,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            max_invitees: 50,
            title_max_len: 100,
            description_max_len: 10000,
        }
    }
}

impl CalendarConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.max_invitees > 0, "calendar.max_invitees={} is outside its valid domain", self.max_invitees);
        anyhow::ensure!(self.title_max_len > 0, "calendar.title_max_len={} is outside its valid domain", self.title_max_len);
        anyhow::ensure!(self.description_max_len > 0, "calendar.description_max_len={} is outside its valid domain", self.description_max_len);
        Ok(())
    }
}

/// Space configuration.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpaceConfig {
    /// Default source: space/snapshot.rs and space/actor.rs persistence and mailbox policy.
    pub position_snapshot_secs: i64,
    /// Default source: space/snapshot.rs and space/actor.rs persistence and mailbox policy.
    pub mailbox_budget: i64,
}

impl Default for SpaceConfig {
    fn default() -> Self {
        Self {
            position_snapshot_secs: 30,
            mailbox_budget: 10000,
        }
    }
}

impl SpaceConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.position_snapshot_secs > 0, "space.position_snapshot_secs={} is outside its valid domain", self.position_snapshot_secs);
        anyhow::ensure!(self.mailbox_budget > 0, "space.mailbox_budget={} is outside its valid domain", self.mailbox_budget);
        Ok(())
    }
}

