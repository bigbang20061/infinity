-- Typed, indexed views of the SDE for the server, derived from the lossless
-- sde_compat.* / sde_native.* tables loaded by sde-import. Re-runnable.

BEGIN;
DROP SCHEMA IF EXISTS sde CASCADE;
CREATE SCHEMA sde;

-- JSON scalar helpers; tolerate ints serialised as floats ("1.0") and nulls.
CREATE FUNCTION sde.ji(j jsonb) RETURNS integer LANGUAGE sql IMMUTABLE AS
$$ SELECT CASE WHEN jsonb_typeof(j) = 'number' THEN (j #>> '{}')::numeric::integer END $$;
CREATE FUNCTION sde.jb(j jsonb) RETURNS bigint LANGUAGE sql IMMUTABLE AS
$$ SELECT CASE WHEN jsonb_typeof(j) = 'number' THEN (j #>> '{}')::numeric::bigint END $$;
CREATE FUNCTION sde.jf(j jsonb) RETURNS double precision LANGUAGE sql IMMUTABLE AS
$$ SELECT CASE WHEN jsonb_typeof(j) = 'number' THEN (j #>> '{}')::double precision END $$;
CREATE FUNCTION sde.jbool(j jsonb) RETURNS boolean LANGUAGE sql IMMUTABLE AS
$$ SELECT CASE jsonb_typeof(j) WHEN 'boolean' THEN (j #>> '{}')::boolean
                               WHEN 'number'  THEN (j #>> '{}')::numeric <> 0 END $$;
-- Localised text: {"en": "..."} objects or plain strings.
CREATE FUNCTION sde.jt(j jsonb) RETURNS text LANGUAGE sql IMMUTABLE AS
$$ SELECT CASE jsonb_typeof(j) WHEN 'object' THEN j ->> 'en' WHEN 'string' THEN j #>> '{}' END $$;

-- ---------------------------------------------------------------- inventory
CREATE TABLE sde.inv_categories AS
SELECT key::integer AS category_id, sde.jt(data->'name') AS name,
       sde.jbool(data->'published') AS published, sde.ji(data->'iconID') AS icon_id, data
FROM sde_compat.categories;
ALTER TABLE sde.inv_categories ADD PRIMARY KEY (category_id);

CREATE TABLE sde.inv_groups AS
SELECT key::integer AS group_id, sde.ji(data->'categoryID') AS category_id, sde.jt(data->'name') AS name,
       sde.jbool(data->'published') AS published, sde.jbool(data->'anchorable') AS anchorable,
       sde.jbool(data->'anchored') AS anchored, sde.jbool(data->'fittableNonSingleton') AS fittable_non_singleton,
       sde.jbool(data->'useBasePrice') AS use_base_price, sde.ji(data->'iconID') AS icon_id, data
FROM sde_compat.groups;
ALTER TABLE sde.inv_groups ADD PRIMARY KEY (group_id);
CREATE INDEX ON sde.inv_groups (category_id);

CREATE TABLE sde.inv_types AS
SELECT t.key::integer AS type_id, sde.ji(t.data->'groupID') AS group_id, g.category_id,
       sde.jt(t.data->'name') AS name, sde.jt(t.data->'description') AS description,
       sde.jf(t.data->'mass') AS mass, sde.jf(t.data->'volume') AS volume,
       sde.jf(t.data->'capacity') AS capacity, sde.jf(t.data->'radius') AS radius,
       sde.ji(t.data->'portionSize') AS portion_size, sde.jf(t.data->'basePrice') AS base_price,
       sde.jbool(t.data->'published') AS published, sde.ji(t.data->'marketGroupID') AS market_group_id,
       sde.ji(t.data->'raceID') AS race_id, sde.ji(t.data->'graphicID') AS graphic_id,
       sde.ji(t.data->'iconID') AS icon_id, sde.ji(t.data->'soundID') AS sound_id, t.data
FROM sde_compat.types t LEFT JOIN sde.inv_groups g ON g.group_id = sde.ji(t.data->'groupID');
ALTER TABLE sde.inv_types ADD PRIMARY KEY (type_id);
CREATE INDEX ON sde.inv_types (group_id);
CREATE INDEX ON sde.inv_types (category_id);

CREATE TABLE sde.inv_type_materials AS
SELECT m.key::integer AS type_id, sde.ji(e->'materialTypeID') AS material_type_id, sde.jb(e->'quantity') AS quantity
FROM sde_compat.typematerials m, jsonb_array_elements(m.data->'materials') e;
ALTER TABLE sde.inv_type_materials ADD PRIMARY KEY (type_id, material_type_id);

CREATE TABLE sde.icons AS
SELECT key::integer AS icon_id, data->>'iconFile' AS icon_file FROM sde_compat.icons;
ALTER TABLE sde.icons ADD PRIMARY KEY (icon_id);

CREATE TABLE sde.blueprints AS
SELECT key::integer AS blueprint_type_id, sde.ji(data->'maxProductionLimit') AS max_production_limit,
       data->'activities' AS activities
FROM sde_compat.blueprints;
ALTER TABLE sde.blueprints ADD PRIMARY KEY (blueprint_type_id);

-- -------------------------------------------------------------------- dogma
CREATE TABLE sde.dogma_attributes AS
SELECT key::integer AS attribute_id, data->>'name' AS name, sde.ji(data->'dataType') AS data_type,
       sde.jf(data->'defaultValue') AS default_value, sde.jbool(data->'highIsGood') AS high_is_good,
       sde.jbool(data->'stackable') AS stackable, sde.jbool(data->'published') AS published,
       sde.ji(data->'attributeCategoryID') AS category_id, sde.ji(data->'unitID') AS unit_id,
       sde.ji(data->'iconID') AS icon_id, sde.jt(data->'displayName') AS display_name, data
FROM sde_compat.dogmaattributes;
ALTER TABLE sde.dogma_attributes ADD PRIMARY KEY (attribute_id);
CREATE INDEX ON sde.dogma_attributes (name);  -- not unique: CCP ships duplicate names

CREATE TABLE sde.dogma_effects AS
SELECT key::integer AS effect_id, data->>'name' AS name, data->>'guid' AS guid,
       sde.ji(data->'effectCategoryID') AS effect_category,
       sde.jbool(data->'isOffensive') AS is_offensive, sde.jbool(data->'isAssistance') AS is_assistance,
       sde.jbool(data->'isWarpSafe') AS is_warp_safe, sde.jbool(data->'disallowAutoRepeat') AS disallow_auto_repeat,
       sde.jbool(data->'published') AS published, sde.jbool(data->'electronicChance') AS electronic_chance,
       sde.jbool(data->'propulsionChance') AS propulsion_chance, sde.jbool(data->'rangeChance') AS range_chance,
       sde.ji(data->'durationAttributeID') AS duration_attribute_id,
       sde.ji(data->'dischargeAttributeID') AS discharge_attribute_id,
       sde.ji(data->'rangeAttributeID') AS range_attribute_id,
       sde.ji(data->'falloffAttributeID') AS falloff_attribute_id,
       sde.ji(data->'trackingSpeedAttributeID') AS tracking_speed_attribute_id,
       sde.ji(data->'resistanceAttributeID') AS resistance_attribute_id,
       sde.ji(data->'npcUsageChanceAttributeID') AS npc_usage_chance_attribute_id,
       sde.ji(data->'npcActivationChanceAttributeID') AS npc_activation_chance_attribute_id,
       sde.ji(data->'fittingUsageChanceAttributeID') AS fitting_usage_chance_attribute_id,
       sde.ji(data->'distribution') AS distribution, data->>'sfxName' AS sfx_name,
       COALESCE(data->'modifierInfo', '[]'::jsonb) AS modifier_info, data
FROM sde_compat.dogmaeffects;
ALTER TABLE sde.dogma_effects ADD PRIMARY KEY (effect_id);

CREATE TABLE sde.dogma_effect_modifiers AS
SELECT e.key::integer AS effect_id, m.ord::integer - 1 AS idx, m.v->>'func' AS func, m.v->>'domain' AS domain,
       sde.ji(m.v->'operation') AS operation, sde.ji(m.v->'modifiedAttributeID') AS modified_attribute_id,
       sde.ji(m.v->'modifyingAttributeID') AS modifying_attribute_id, sde.ji(m.v->'groupID') AS group_id,
       sde.ji(m.v->'skillTypeID') AS skill_type_id, sde.ji(m.v->'effectID') AS ref_effect_id
FROM sde_compat.dogmaeffects e, jsonb_array_elements(COALESCE(e.data->'modifierInfo', '[]')) WITH ORDINALITY m(v, ord);
ALTER TABLE sde.dogma_effect_modifiers ADD PRIMARY KEY (effect_id, idx);

CREATE TABLE sde.type_attributes AS
SELECT d.key::integer AS type_id, sde.ji(a->'attributeID') AS attribute_id, sde.jf(a->'value') AS value
FROM sde_compat.typedogma d, jsonb_array_elements(d.data->'dogmaAttributes') a;
ALTER TABLE sde.type_attributes ADD PRIMARY KEY (type_id, attribute_id);
CREATE INDEX ON sde.type_attributes (attribute_id);

CREATE TABLE sde.type_effects AS
SELECT d.key::integer AS type_id, sde.ji(e->'effectID') AS effect_id, bool_or(COALESCE(sde.jbool(e->'isDefault'), false)) AS is_default
FROM sde_compat.typedogma d, jsonb_array_elements(d.data->'dogmaEffects') e
GROUP BY 1, 2;  -- CCP data repeats a few (type, effect) pairs verbatim
ALTER TABLE sde.type_effects ADD PRIMARY KEY (type_id, effect_id);
CREATE INDEX ON sde.type_effects (effect_id);

-- ---------------------------------------------------------------------- map
CREATE TABLE sde.map_regions AS
SELECT key::integer AS region_id, sde.jt(data->'name') AS name, sde.ji(data->'factionID') AS faction_id,
       sde.ji(data->'nebulaID') AS nebula_id, sde.ji(data->'wormholeClassID') AS wormhole_class_id,
       sde.jf(data#>'{position,x}') AS x, sde.jf(data#>'{position,y}') AS y, sde.jf(data#>'{position,z}') AS z, data
FROM sde_compat.mapregions;
ALTER TABLE sde.map_regions ADD PRIMARY KEY (region_id);

CREATE TABLE sde.map_constellations AS
SELECT key::integer AS constellation_id, sde.ji(data->'regionID') AS region_id, sde.jt(data->'name') AS name,
       sde.ji(data->'factionID') AS faction_id, sde.ji(data->'wormholeClassID') AS wormhole_class_id,
       sde.jf(data#>'{position,x}') AS x, sde.jf(data#>'{position,y}') AS y, sde.jf(data#>'{position,z}') AS z, data
FROM sde_compat.mapconstellations;
ALTER TABLE sde.map_constellations ADD PRIMARY KEY (constellation_id);

CREATE TABLE sde.map_solar_systems AS
SELECT key::integer AS solar_system_id, sde.ji(data->'constellationID') AS constellation_id,
       sde.ji(data->'regionID') AS region_id, sde.jt(data->'name') AS name,
       sde.jf(data->'securityStatus') AS security, data->>'securityClass' AS security_class,
       sde.ji(data->'factionID') AS faction_id, sde.ji(data->'starID') AS star_id,
       sde.jf(data->'radius') AS radius, sde.jf(data->'luminosity') AS luminosity,
       sde.jbool(data->'border') AS border, sde.jbool(data->'corridor') AS corridor, sde.jbool(data->'hub') AS hub,
       sde.jbool(data->'international') AS international, sde.jbool(data->'regional') AS regional,
       sde.jf(data#>'{position,x}') AS x, sde.jf(data#>'{position,y}') AS y, sde.jf(data#>'{position,z}') AS z, data
FROM sde_compat.mapsolarsystems;
ALTER TABLE sde.map_solar_systems ADD PRIMARY KEY (solar_system_id);
CREATE INDEX ON sde.map_solar_systems (constellation_id);
CREATE INDEX ON sde.map_solar_systems (region_id);
CREATE INDEX ON sde.map_solar_systems (lower(name));

CREATE TABLE sde.map_stargates AS
SELECT key::integer AS stargate_id, sde.ji(data->'solarSystemID') AS solar_system_id, sde.ji(data->'typeID') AS type_id,
       sde.ji(data#>'{destination,stargateID}') AS dest_stargate_id,
       sde.ji(data#>'{destination,solarSystemID}') AS dest_solar_system_id,
       sde.jf(data#>'{position,x}') AS x, sde.jf(data#>'{position,y}') AS y, sde.jf(data#>'{position,z}') AS z
FROM sde_compat.mapstargates;
ALTER TABLE sde.map_stargates ADD PRIMARY KEY (stargate_id);
CREATE INDEX ON sde.map_stargates (solar_system_id);

CREATE TABLE sde.map_jumps AS
SELECT DISTINCT solar_system_id AS from_solar_system_id, dest_solar_system_id AS to_solar_system_id
FROM sde.map_stargates WHERE dest_solar_system_id IS NOT NULL;
ALTER TABLE sde.map_jumps ADD PRIMARY KEY (from_solar_system_id, to_solar_system_id);

CREATE TABLE sde.sta_stations AS
SELECT s.key::integer AS station_id, sde.ji(s.data->'solarSystemID') AS solar_system_id,
       ss.constellation_id, ss.region_id, sde.ji(s.data->'typeID') AS type_id, sde.ji(s.data->'ownerID') AS owner_id,
       sde.ji(s.data->'operationID') AS operation_id, sde.jb(s.data->'orbitID') AS orbit_id,
       sde.ji(s.data->'celestialIndex') AS celestial_index, sde.ji(s.data->'orbitIndex') AS orbit_index,
       sde.jbool(s.data->'useOperationName') AS use_operation_name,
       sde.jf(s.data->'reprocessingEfficiency') AS reprocessing_efficiency,
       sde.jf(s.data->'reprocessingStationsTake') AS reprocessing_stations_take,
       sde.ji(s.data->'reprocessingHangarFlag') AS reprocessing_hangar_flag,
       ss.security,
       sde.jf(s.data#>'{position,x}') AS x, sde.jf(s.data#>'{position,y}') AS y, sde.jf(s.data#>'{position,z}') AS z
FROM sde_compat.npcstations s LEFT JOIN sde.map_solar_systems ss ON ss.solar_system_id = sde.ji(s.data->'solarSystemID');
ALTER TABLE sde.sta_stations ADD PRIMARY KEY (station_id);
CREATE INDEX ON sde.sta_stations (solar_system_id);
CREATE INDEX ON sde.sta_stations (owner_id);

CREATE TABLE sde.sta_operations AS
SELECT key::integer AS operation_id, sde.jt(data->'operationName') AS name, sde.ji(data->'activityID') AS activity_id,
       ARRAY(SELECT sde.ji(v) FROM jsonb_array_elements(COALESCE(data->'services', '[]')) v) AS service_ids, data
FROM sde_compat.stationoperations;
ALTER TABLE sde.sta_operations ADD PRIMARY KEY (operation_id);

-- Every in-space static object a solar system ballpark needs, mapDenormalize-style.
CREATE TABLE sde.map_celestials AS
WITH c AS (
    SELECT key::bigint AS item_id, data, NULL::bigint AS orbit_id_override FROM sde_compat.mapstars
    UNION ALL SELECT key::bigint, data, NULL FROM sde_compat.mapplanets
    UNION ALL SELECT key::bigint, data, NULL FROM sde_compat.mapmoons
    UNION ALL SELECT key::bigint, data, NULL FROM sde_compat.mapasteroidbelts
    UNION ALL SELECT key::bigint, data, NULL FROM sde_compat.mapstargates
    UNION ALL SELECT key::bigint, data, NULL FROM sde_compat.npcstations
)
SELECT c.item_id, sde.ji(c.data->'typeID') AS type_id, t.group_id, sde.ji(c.data->'solarSystemID') AS solar_system_id,
       ss.constellation_id, ss.region_id, sde.jb(c.data->'orbitID') AS orbit_id,
       sde.ji(c.data->'celestialIndex') AS celestial_index, sde.ji(c.data->'orbitIndex') AS orbit_index,
       sde.jf(c.data->'radius') AS radius, ss.security,
       COALESCE(sde.jf(c.data#>'{position,x}'), 0) AS x, COALESCE(sde.jf(c.data#>'{position,y}'), 0) AS y,
       COALESCE(sde.jf(c.data#>'{position,z}'), 0) AS z
FROM c
LEFT JOIN sde.inv_types t ON t.type_id = sde.ji(c.data->'typeID')
LEFT JOIN sde.map_solar_systems ss ON ss.solar_system_id = sde.ji(c.data->'solarSystemID');
ALTER TABLE sde.map_celestials ADD PRIMARY KEY (item_id);
CREATE INDEX ON sde.map_celestials (solar_system_id);
CREATE INDEX ON sde.map_celestials (group_id);

-- ------------------------------------------------------------ factions/chars
CREATE TABLE sde.chr_factions AS
SELECT key::integer AS faction_id, sde.jt(data->'name') AS name, sde.ji(data->'corporationID') AS corporation_id,
       sde.ji(data->'militiaCorporationID') AS militia_corporation_id, sde.ji(data->'solarSystemID') AS solar_system_id,
       sde.ji(data->'iconID') AS icon_id, sde.jf(data->'sizeFactor') AS size_factor, data
FROM sde_compat.factions;
ALTER TABLE sde.chr_factions ADD PRIMARY KEY (faction_id);

CREATE TABLE sde.chr_races AS
SELECT key::integer AS race_id, sde.jt(data->'name') AS name, sde.ji(data->'shipTypeID') AS ship_type_id,
       sde.ji(data->'iconID') AS icon_id, data
FROM sde_compat.races;
ALTER TABLE sde.chr_races ADD PRIMARY KEY (race_id);

CREATE TABLE sde.chr_bloodlines AS
SELECT key::integer AS bloodline_id, sde.ji(data->'raceID') AS race_id, sde.jt(data->'name') AS name,
       sde.ji(data->'corporationID') AS corporation_id, sde.ji(data->'charisma') AS charisma,
       sde.ji(data->'intelligence') AS intelligence, sde.ji(data->'memory') AS memory,
       sde.ji(data->'perception') AS perception, sde.ji(data->'willpower') AS willpower, data
FROM sde_compat.bloodlines;
ALTER TABLE sde.chr_bloodlines ADD PRIMARY KEY (bloodline_id);

CREATE TABLE sde.crp_npc_corporations AS
SELECT key::integer AS corporation_id, sde.jt(data->'name') AS name, data->>'tickerName' AS ticker,
       sde.ji(data->'ceoID') AS ceo_id, sde.ji(data->'factionID') AS faction_id, sde.ji(data->'raceID') AS race_id,
       sde.ji(data->'solarSystemID') AS solar_system_id, sde.ji(data->'stationID') AS station_id,
       sde.jf(data->'taxRate') AS tax_rate, sde.ji(data->'memberLimit') AS member_limit,
       data->>'size' AS size, data->>'extent' AS extent, sde.jbool(data->'deleted') AS deleted, data
FROM sde_compat.npccorporations;
ALTER TABLE sde.crp_npc_corporations ADD PRIMARY KEY (corporation_id);

CREATE TABLE sde.npc_characters AS
SELECT key::integer AS character_id, data FROM sde_compat.npccharacters;
ALTER TABLE sde.npc_characters ADD PRIMARY KEY (character_id);

CREATE TABLE sde.build_info AS
SELECT sde.ji(data->'buildNumber') AS build_number, data->>'releaseDate' AS release_date
FROM sde_compat._sde WHERE key = 'sde';

ANALYZE;
COMMIT;
