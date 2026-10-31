//! Numeric classification of dogma effects for the shared effects lifecycle.
//!
//! IDs and modifier rows come from the SDE; client dogma effect constants and
//! pythonEffects supply corresponding behavior names. Later-wave kinds are
//! classified here but are not implemented by this registry.
pub(crate) mod registry;
pub(crate) mod environment;
pub(crate) mod notify;
pub(crate) mod unsupported;
pub(crate) mod propulsion;
pub(crate) mod weapons;
pub(crate) mod hacking;
pub(crate) mod lifecycle;
mod repair;
mod mining;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JammingType {
    Webify, Scramble, Disrupt, Paint, Damp, TrackingDisrupt, GuidanceDisrupt,
    SensorBoost, TrackingBoost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectKind {
    Modifiers, Turret, Missile, Smartbomb, Vorton,
    LocalRepair { layer: usize, amount_attr: i32 },
    FueledLocalRepair { layer: usize, amount_attr: i32, charge_bonus_attr: Option<i32> },
    RemoteRepair { layer: usize },
    CapTransfer, Neutralizer, Nosferatu, CapBooster, Propulsion, Mining,
    Projected { jamming: JammingType }, Ecm, BurstJammer, Cloak,
    MicroJumpDrive, Salvage, Tractor, CargoScan, ShipScan, SurveyScan,
    AutoTarget, PassiveTarget, Bomb, ProbeLaunch, Hacking, Online,
}
