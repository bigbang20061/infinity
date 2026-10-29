//! Game-data constants shared by server modules, grouped by source.
//!
//! IDs follow the SDE and the client's inventorycommon, dogma and corporation
//! constants. FILETIME durations use Windows 100 ns ticks; AU is in metres.
//! Runtime configuration, formulas and module-owned wire protocol constants do
//! not belong here. This module provides no data lookup or runtime state.

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
/// SDE invTypes: Hacking skill.
pub const SKILL_HACKING: i32 = 21718;
/// SDE invTypes: Archaeology skill.
pub const SKILL_ARCHAEOLOGY: i32 = 13278;

// SDE dogma attribute and effect IDs.
/// SDE dgmAttributeTypes: mass.
pub const ATTR_MASS: i32 = 4;
/// SDE dgmAttributeTypes: capacity.
pub const ATTR_CAPACITY: i32 = 38;
/// SDE dgmAttributeTypes: volume.
pub const ATTR_VOLUME: i32 = 161;
/// SDE dgmAttributeTypes: radius.
pub const ATTR_RADIUS: i32 = 162;
/// SDE dgmAttributeTypes: maxVelocity (also used for missiles).
pub const ATTR_MAX_VELOCITY: i32 = 37;
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
/// Client appConst.accountingKeyCash: personal cash and the master corporation wallet.
pub const ACCOUNT_CASH: i32 = 1000;
/// Client appConst.rentalPeriodOffice, in days.
pub const RENTAL_DAYS: i64 = 30;
/// Server market policy: NPC buy orders pay this fraction of the NPC sell price.
pub const NPC_BID_RATIO: f64 = 0.8;
