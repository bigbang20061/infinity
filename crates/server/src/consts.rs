//! Game-data constants shared by server modules, grouped by source.
//!
//! IDs follow the SDE and the client's inventorycommon, dogma and corporation
//! constants. FILETIME durations use Windows 100 ns ticks; AU is in metres.
//! Runtime configuration, formulas and module-owned wire protocol constants do
//! not belong here. Shared gateway protocol units are included. This module
//! provides no data lookup or runtime state.

// FILETIME durations and timestamps.
/// Windows FILETIME epoch offset: 100 ns ticks from 1601-01-01 to 1970-01-01.
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Windows FILETIME ticks per second (100 ns per tick).
pub const FILETIME_SECOND: i64 = 10_000_000;
/// Client blue time convention: FILETIME ticks per minute.
pub const FILETIME_MINUTE: i64 = 60 * FILETIME_SECOND;
/// Client blue time convention: FILETIME ticks per hour.
pub const FILETIME_HOUR: i64 = 60 * FILETIME_MINUTE;
/// Client blue time convention: FILETIME ticks per day.
pub const FILETIME_DAY: i64 = 24 * FILETIME_HOUR;
/// Server subscription policy: expiry at 2100-12-31, expressed as FILETIME.
pub const FAR_FUTURE: i64 = 157_766_976_000_000_000;
// Standard time units.
/// Standard time unit: seconds in a minute.
pub const SECONDS_PER_MINUTE: i64 = 60;
/// Standard time unit: seconds in an hour.
pub const SECONDS_PER_HOUR: i64 = 3600;

// Existing damage representation.
/// SDE/dogma damage attribute grouping: EM, thermal, kinetic and explosive.
pub const DAMAGE_TYPE_COUNT: usize = 4;
/// Ship damage layer model: shield, armor and structure.
pub const DAMAGE_LAYER_COUNT: usize = 3;


// Distances.
/// Server distance convention: the IAU astronomical unit, in metres.
pub const AU: f64 = 149_597_870_700.0;

// SDE inventory type, group and category IDs.
/// SDE invTypes: Character (Gallente), used for player owners.
pub const TYPE_CHARACTER: i32 = 1373;
/// SDE invTypes: Solar System.
pub const TYPE_SOLAR_SYSTEM: i32 = 5;
/// SDE invTypes: Capsule.
pub const TYPE_CAPSULE: i32 = 670;
/// SDE invGroups: Planet.
pub const GROUP_PLANET: i32 = 7;
/// SDE invGroups: Asteroid Belt.
pub const GROUP_ASTEROID_BELT: i32 = 9;
/// SDE invCategories: Ship.
pub const CATEGORY_SHIP: i32 = 6;
/// SDE invCategories: Charge.
pub const CATEGORY_CHARGE: i32 = 8;
/// SDE invTypes: Hacking skill.
pub const SKILL_HACKING: i32 = 21718;
/// SDE invTypes: Archaeology skill.
pub const SKILL_ARCHAEOLOGY: i32 = 13278;

// SDE faction IDs.
/// SDE chrFactions: Caldari State.
pub const FACTION_CALDARI: i64 = 500001;
/// SDE chrFactions: Minmatar Republic.
pub const FACTION_MINMATAR: i64 = 500002;
/// SDE chrFactions: Amarr Empire.
pub const FACTION_AMARR: i64 = 500003;
/// SDE chrFactions: Gallente Federation.
pub const FACTION_GALLENTE: i64 = 500004;
/// SDE chrFactions: Guristas Pirates.
pub const FACTION_GURISTAS: i64 = 500010;
/// SDE chrFactions: Angel Cartel.
pub const FACTION_ANGEL_CARTEL: i64 = 500011;

// SDE dogma attribute and effect IDs.
/// SDE dgmAttributeTypes: mass.
pub const ATTR_MASS: i32 = 4;
/// SDE dgmAttributeTypes: hp (structure hitpoint capacity).
pub const ATTR_STRUCTURE_HP: i32 = 9;
/// SDE dgmAttributeTypes: capacity.
pub const ATTR_CAPACITY: i32 = 38;
/// SDE dgmAttributeTypes: volume.
pub const ATTR_VOLUME: i32 = 161;
/// SDE dgmAttributeTypes: radius.
pub const ATTR_RADIUS: i32 = 162;
/// SDE dgmAttributeTypes: maxVelocity (also used for missiles).
pub const ATTR_MAX_VELOCITY: i32 = 37;
/// SDE dgmAttributeTypes: agility.
pub const ATTR_AGILITY: i32 = 70;
/// SDE dgmAttributeTypes: signatureRadius.
pub const ATTR_SIGNATURE_RADIUS: i32 = 552;
/// SDE dgmAttributeTypes: maxRange.
pub const ATTR_MAX_RANGE: i32 = 54;
/// SDE dgmAttributeTypes: quantity.
pub const ATTR_QUANTITY: i32 = 805;
/// SDE dgmAttributeTypes: isOnline.
pub const ATTR_IS_ONLINE: i32 = 2;
/// SDE dgmAttributeTypes: explosionDelay (also used for probe lifetime).
pub const ATTR_EXPLOSION_DELAY: i32 = 281;
/// SDE dgmAttributeTypes: capacitorCapacity.
pub const ATTR_CAPACITOR_CAPACITY: i32 = 482;
/// SDE dgmAttributeTypes: shieldCapacity.
pub const ATTR_SHIELD_CAPACITY: i32 = 263;
/// SDE dgmAttributeTypes: charge.
pub const ATTR_CHARGE: i32 = 18;
/// SDE dgmAttributeTypes: shieldCharge.
pub const ATTR_SHIELD_CHARGE: i32 = 264;
/// SDE dgmAttributeTypes: armorDamage.
pub const ATTR_ARMOR_DAMAGE: i32 = 266;
/// SDE dgmAttributeTypes: damage (hull damage), not hitpoint capacity hp (9).
pub const ATTR_HULL_DAMAGE: i32 = 3;
/// SDE dgmAttributeTypes: requiredSkill1.
pub const ATTR_REQUIRED_SKILL_1: i32 = 182;
/// SDE dgmEffects: online; an effect ID, not an effect category.
pub const EFFECT_ONLINE: i64 = 16;
/// SDE dgmEffects: moduleBonusMicrowarpdrive.
pub const EFFECT_MICROWARPDRIVE: i32 = 6730;
/// SDE dgmEffects: moduleBonusAfterburner.
pub const EFFECT_AFTERBURNER: i32 = 6731;

// Client inventory flags, locations and row-change keys.
/// Client inventorycommon.const.flagHangar.
pub const FLAG_HANGAR: i32 = 4;
/// Client inventorycommon.const.flagCargo.
pub const FLAG_CARGO: i32 = 5;
/// Client inventorycommon.const.flagLoSlot0.
pub const FLAG_FITTING_FIRST: i32 = 11;
/// Client inventorycommon.const.flagHiSlot7.
pub const FLAG_FITTING_LAST: i32 = 34;
/// Client inventorycommon.const.flagRigSlot0.
pub const FLAG_RIG_FIRST: i32 = 92;
/// Client inventorycommon.const.flagRigSlot7.
pub const FLAG_RIG_LAST: i32 = 99;
/// Client inventorycommon.const.flagSubSystemSlot0.
pub const FLAG_SUBSYSTEM_FIRST: i32 = 125;
/// Client inventorycommon.const.flagSubSystemSlot7.
pub const FLAG_SUBSYSTEM_LAST: i32 = 132;
/// Client inventorycommon.const.flagPilot.
pub const FLAG_PILOT: i32 = 57;
/// Client inventorycommon.const.flagCorpSAG1; other divisions follow it.
pub const FLAG_CORP_SAG1: i32 = 115;
/// Client inventorycommon.const.locationJunkyard: deleted items.
pub const LOCATION_JUNKYARD: i64 = 10;
/// Client carbon const.ixLocationID: inventory row location column.
pub const IX_LOCATION_ID: i64 = 3;
/// Client carbon const.ixFlag: inventory row flag column.
pub const IX_FLAG: i64 = 4;
/// Client inventorycommon.const.ixStackSize: inventory stack-size change key.
pub const IX_STACK_SIZE: i64 = 9;
/// Client inventorycommon.const.ixSingleton: inventory singleton change key.
pub const IX_SINGLETON: i64 = 10;

// Client inventory singleton quantity encoding.
/// Client inventory singleton encoding: blueprint copy.
pub const BLUEPRINT_COPY_QUANTITY: i64 = -2;
/// Client inventory singleton encoding: original or singleton item.
pub const SINGLETON_QUANTITY: i64 = -1;

// Server corporation and alliance creation validation policy.
/// Minimum corporation or alliance name length.
pub const ORG_NAME_MIN_LENGTH: usize = 4;
/// Maximum corporation or alliance name length.
pub const ORG_NAME_MAX_LENGTH: usize = 50;
/// Minimum corporation or alliance short name length.
pub const ORG_SHORT_NAME_MIN_LENGTH: usize = 2;
/// Maximum corporation or alliance short name length.
pub const ORG_SHORT_NAME_MAX_LENGTH: usize = 5;

// Client corporation public row conventions.
/// Client corporation row: hangar and wallet division name count.
pub const CORP_DIVISION_NAME_COUNT: usize = 14;
/// Client corporation public row: unlimited member limit.
pub const CORPORATION_UNLIMITED_MEMBER_LIMIT: i64 = -1;
/// Client corporation public row: NPC corporation type.
pub const CORPORATION_TYPE_NPC: i64 = 1;
/// Client corporation public row: player corporation type.
pub const CORPORATION_TYPE_PLAYER: i64 = 2;

// Client industry job and blueprint conventions.
/// Client industry job status: installed.
pub const INDUSTRY_JOB_STATUS_INSTALLED: i32 = 1;
/// Client blueprint original convention: infinite runs, distinct from quantity encoding.
pub const BLUEPRINT_INFINITE_RUNS: i32 = -1;

// Client corporation and service roles.
/// Client appConst.corpRoleDirector.
pub const ROLE_DIRECTOR: i64 = 1;
/// Client appConst.corpRoleAccountant.
pub const ROLE_ACCOUNTANT: i64 = 256;
/// Client carbon/common/script/sys/serviceConst.py: ROLE_PLAYER | ROLE_LOGIN.
pub const ROLE_PLAYER_LOGIN: i64 = 2305843009213693952 | 4611686018427387904;
/// Client carbon serviceConst: ROLE_GML | ROLE_GMH | ROLE_LEGIONEER | ROLE_WORLDMOD.
pub const GM_ROLES: i64 = 18014398509481984 | 9007199254740992 | 262144 | 4096;

// Client wallet and office constants; server market policy.
/// Gateway PLEX vault protocol: one PLEX is 100 hundredths.
pub(crate) const PLEX_HUNDREDTHS: i64 = 100;
/// Largest character ISK balance displayed correctly by the Infinity client (build 3653293),
/// measured on the client; exactly representable as f64. The database keeps 36 digits.
pub(crate) const CLIENT_MAX_ISK: f64 = 1_000_000_000_000_000_000.0;
/// Largest corporation wallet balance sent to the client; the maintainer observed
/// balances above `CLIENT_MAX_ISK` display correctly. Corporation wallet FmtAmt
/// (`carbon/common/script/util/format.py`) falls back to scientific-notation float
/// text above `appConst.maxLong`. This is the largest f64 not above maxLong;
/// `i64::MAX as f64` rounds above it.
pub(crate) const CLIENT_MAX_CORP_ISK: f64 = 9_223_372_036_854_774_784.0;
/// Largest PLEX balance displayed correctly by the Infinity client (build 3653293),
/// measured on the client; `carbon/common/script/util/format.py` FmtAmt short formats
/// reject amounts >= 1e14 (`WhatKindOfAmountIsThis`). Gateway hundredths
/// (`CLIENT_MAX_PLEX * PLEX_HUNDREDTHS`) remain well inside sint64.
pub(crate) const CLIENT_MAX_PLEX: i64 = 99_999_999_999_999;
/// Client appConst.accountingKeyCash: personal cash and the master corporation wallet.
pub const ACCOUNT_CASH: i32 = 1000;
/// Client appConst.rentalPeriodOffice, in days.
pub const RENTAL_DAYS: i64 = 30;
/// Server market policy: NPC buy orders pay this fraction of the NPC sell price.
pub const NPC_BID_RATIO: f64 = 0.8;
