-- Mutable game state (accounts, characters, items...). Idempotent; applied by
-- the server at startup. Static data lives in schema `sde` (sde_typed.sql).

CREATE SCHEMA IF NOT EXISTS game;

CREATE TABLE IF NOT EXISTS game.accounts (
    user_id       bigserial PRIMARY KEY,
    user_name     text NOT NULL,
    -- macho.PasswordHash(): SHA-1 chain over UTF-16 password + lower(user name).
    password_hash bytea NOT NULL,
    role          bigint NOT NULL,
    user_type     integer NOT NULL DEFAULT 20,  -- const.userTypePBC
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login    timestamptz
);
CREATE UNIQUE INDEX IF NOT EXISTS accounts_user_name_idx ON game.accounts (lower(user_name));
-- User IDs in the live game are large; start well clear of NPC/static ID ranges.
SELECT setval('game.accounts_user_id_seq', GREATEST((SELECT max(user_id) FROM game.accounts), 10000000), true);

CREATE TABLE IF NOT EXISTS game.characters (
    character_id     bigserial PRIMARY KEY,
    user_id          bigint NOT NULL REFERENCES game.accounts,
    name             text NOT NULL,
    race_id          integer NOT NULL,
    bloodline_id     integer NOT NULL,
    gender_id        integer NOT NULL,
    ancestry_id      integer,
    school_id        integer,
    corporation_id   bigint NOT NULL,
    station_id       bigint,
    solar_system_id  bigint,
    constellation_id bigint,
    region_id        bigint,
    balance          numeric(20, 2) NOT NULL DEFAULT 0,
    skill_points     bigint NOT NULL DEFAULT 0,
    -- Marshalled paperdoll appearance / portrait blobs exactly as the client sent them.
    appearance       bytea,
    portrait         bytea,
    created_at       timestamptz NOT NULL DEFAULT now(),
    logoff_at        timestamptz NOT NULL DEFAULT now(),
    deleted          boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX IF NOT EXISTS characters_name_idx ON game.characters (lower(name)) WHERE NOT deleted;
CREATE INDEX IF NOT EXISTS characters_user_idx ON game.characters (user_id);
CREATE TABLE IF NOT EXISTS game.items (
    item_id     bigserial PRIMARY KEY,
    type_id     integer NOT NULL,
    owner_id    bigint NOT NULL,
    location_id bigint NOT NULL,
    flag_id     integer NOT NULL,
    quantity    integer NOT NULL DEFAULT 1,
    singleton   boolean NOT NULL DEFAULT false,
    custom_info text,
    name        text
);
CREATE INDEX IF NOT EXISTS items_location_idx ON game.items (location_id, flag_id);
CREATE INDEX IF NOT EXISTS items_owner_idx ON game.items (owner_id);
-- idCheckers.IsPlayerItem: >= 100,000,000; start far above character IDs.
SELECT setval('game.items_item_id_seq', GREATEST((SELECT max(item_id) FROM game.items), 1000000000000), true);

ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS ship_id bigint;
-- Position of items in space (location_id = solar system).
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS x double precision;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS y double precision;
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS z double precision;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS free_skill_points bigint NOT NULL DEFAULT 0;
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS home_station_id bigint;
UPDATE game.characters SET home_station_id = station_id WHERE home_station_id IS NULL AND station_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS game.skills (
    character_id bigint NOT NULL REFERENCES game.characters,
    type_id      integer NOT NULL,
    level        integer NOT NULL,
    skill_points bigint NOT NULL,
    PRIMARY KEY (character_id, type_id)
);

-- Active training queue; times are FILETIME (100ns since 1601) like the client's.
CREATE TABLE IF NOT EXISTS game.skill_queue (
    character_id bigint NOT NULL REFERENCES game.characters,
    position     integer NOT NULL,
    type_id      integer NOT NULL,
    to_level     integer NOT NULL,
    start_sp     bigint NOT NULL,
    dest_sp      bigint NOT NULL,
    start_time   bigint,
    end_time     bigint,
    PRIMARY KEY (character_id, position)
);

-- Player character IDs live in the 90,000,000+ range (idCheckers.IsEvePlayerCharacter).
SELECT setval('game.characters_character_id_seq', GREATEST((SELECT max(character_id) FROM game.characters), 90000000), true);

-- Regional market. char_id NULL marks the NPC orders seeded at the starter stations
-- (sql/market_seed.sql, and buy orders from them in db.rs); issued NULL means "re-issued today".
CREATE TABLE IF NOT EXISTS game.market_orders (
    order_id         bigserial PRIMARY KEY,
    type_id          integer NOT NULL,
    char_id          bigint,
    station_id       bigint NOT NULL,
    solar_system_id  bigint NOT NULL,
    constellation_id bigint NOT NULL,
    region_id        bigint NOT NULL,
    price            double precision NOT NULL,
    vol_entered      integer NOT NULL,
    vol_remaining    integer NOT NULL,
    min_volume       integer NOT NULL DEFAULT 1,
    bid              boolean NOT NULL,
    range            integer NOT NULL DEFAULT -1,  -- marketutil.const.rangeStation
    duration         integer NOT NULL,             -- days
    issued           bigint                        -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_orders_type_region_idx ON game.market_orders (type_id, region_id);
CREATE INDEX IF NOT EXISTS market_orders_station_idx ON game.market_orders (station_id, type_id);
CREATE INDEX IF NOT EXISTS market_orders_system_idx ON game.market_orders (solar_system_id, type_id);
CREATE INDEX IF NOT EXISTS market_orders_region_idx ON game.market_orders (region_id, type_id);
-- ISK a player's buy order holds for its remaining volume.
ALTER TABLE game.market_orders ADD COLUMN IF NOT EXISTS escrow double precision NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS market_orders_char_idx ON game.market_orders (char_id) WHERE char_id IS NOT NULL;

-- Players' orders once filled, expired (state 2) or cancelled (3): marketHistory's orderState.
CREATE TABLE IF NOT EXISTS game.market_order_history (
    order_id      bigint PRIMARY KEY,
    type_id       integer NOT NULL,
    char_id       bigint NOT NULL,
    station_id    bigint NOT NULL,
    region_id     bigint NOT NULL,
    price         double precision NOT NULL,
    vol_entered   integer NOT NULL,
    vol_remaining integer NOT NULL,
    min_volume    integer NOT NULL,
    bid           boolean NOT NULL,
    range         integer NOT NULL,
    duration      integer NOT NULL,
    issued        bigint NOT NULL,  -- FILETIME
    state         integer NOT NULL,
    closed        bigint NOT NULL   -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_order_history_char_idx ON game.market_order_history (char_id, closed);

-- Every fill. An NPC side is the station's owner. Read back as each side's wallet
-- transactions and as the regional price history.
CREATE TABLE IF NOT EXISTS game.market_transactions (
    transaction_id bigserial PRIMARY KEY,
    type_id        integer NOT NULL,
    quantity       integer NOT NULL,
    price          double precision NOT NULL,
    buyer_id       bigint NOT NULL,
    seller_id      bigint NOT NULL,
    station_id     bigint NOT NULL,
    region_id      bigint NOT NULL,
    date           bigint NOT NULL  -- FILETIME
);
CREATE INDEX IF NOT EXISTS market_transactions_buyer_idx ON game.market_transactions (buyer_id, date);
CREATE INDEX IF NOT EXISTS market_transactions_seller_idx ON game.market_transactions (seller_id, date);
CREATE INDEX IF NOT EXISTS market_transactions_history_idx ON game.market_transactions (region_id, type_id, date);

-- Contracts (services/contracts.rs). type/status are appConst.conType*/conStatus*; an
-- auction's buyout is its collateral. Times are FILETIMEs (0: not yet).
CREATE TABLE IF NOT EXISTS game.contracts (
    contract_id      bigserial PRIMARY KEY,
    type             integer NOT NULL,
    issuer_id        bigint NOT NULL,
    issuer_corp_id   bigint NOT NULL,
    assignee_id      bigint NOT NULL DEFAULT 0,
    acceptor_id      bigint NOT NULL DEFAULT 0,
    start_station_id bigint NOT NULL,
    end_station_id   bigint NOT NULL DEFAULT 0,
    price            double precision NOT NULL DEFAULT 0,
    reward           double precision NOT NULL DEFAULT 0,
    collateral       double precision NOT NULL DEFAULT 0,
    title            text NOT NULL DEFAULT '',
    description      text NOT NULL DEFAULT '',
    volume           double precision NOT NULL DEFAULT 0,
    num_days         integer NOT NULL DEFAULT 0,
    status           integer NOT NULL DEFAULT 0,
    date_issued      bigint NOT NULL,
    date_expired     bigint NOT NULL,
    date_accepted    bigint NOT NULL DEFAULT 0,
    date_completed   bigint NOT NULL DEFAULT 0,
    -- Courier: the plastic wrap the acceptor carries.
    package_id       bigint
);
CREATE INDEX IF NOT EXISTS contracts_status_idx ON game.contracts (status, type);
CREATE INDEX IF NOT EXISTS contracts_issuer_idx ON game.contracts (issuer_id);
CREATE INDEX IF NOT EXISTS contracts_acceptor_idx ON game.contracts (acceptor_id);
CREATE INDEX IF NOT EXISTS contracts_assignee_idx ON game.contracts (assignee_id);

-- What a contract holds (in_crate: escrowed items, located at -contract_id) and what an
-- item exchange asks for in return (item_id NULL).
CREATE TABLE IF NOT EXISTS game.contract_items (
    contract_id bigint NOT NULL REFERENCES game.contracts,
    item_id     bigint,
    type_id     integer NOT NULL,
    quantity    integer NOT NULL,
    in_crate    boolean NOT NULL
);
CREATE INDEX IF NOT EXISTS contract_items_contract_idx ON game.contract_items (contract_id);

-- Auction bids; the highest holds its ISK in escrow until outbid or settled.
CREATE TABLE IF NOT EXISTS game.contract_bids (
    bid_id      bigserial PRIMARY KEY,
    contract_id bigint NOT NULL REFERENCES game.contracts,
    bidder_id   bigint NOT NULL,
    amount      double precision NOT NULL,
    date        bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS contract_bids_contract_idx ON game.contract_bids (contract_id, amount DESC);

-- Blueprint research state; a blueprint without a row is ME 0 / TE 0. runs is -1 for an
-- original. Copies are singleton items of quantity -2.
CREATE TABLE IF NOT EXISTS game.blueprints (
    item_id             bigint PRIMARY KEY,
    material_efficiency integer NOT NULL DEFAULT 0,
    time_efficiency     integer NOT NULL DEFAULT 0,
    runs                integer NOT NULL DEFAULT -1
);

-- Industry jobs (services/industry.rs); status is industry.STATUS_*.
CREATE TABLE IF NOT EXISTS game.industry_jobs (
    job_id            bigserial PRIMARY KEY,
    installer_id      bigint NOT NULL,
    owner_id          bigint NOT NULL,
    activity_id       integer NOT NULL,
    blueprint_id      bigint NOT NULL,
    blueprint_type_id integer NOT NULL,
    blueprint_copy    boolean NOT NULL,
    facility_id       bigint NOT NULL,
    solar_system_id   bigint NOT NULL,
    runs              integer NOT NULL,
    licensed_runs     integer NOT NULL DEFAULT 1,
    successful_runs   integer,
    product_type_id   integer,
    optional_type_id  integer,
    cost              double precision NOT NULL,
    probability       double precision NOT NULL DEFAULT 1,
    time_seconds      bigint NOT NULL,
    start_date        bigint NOT NULL,
    end_date          bigint NOT NULL,
    completed_date    bigint,
    completed_by      bigint,
    status            integer NOT NULL
);
CREATE INDEX IF NOT EXISTS industry_jobs_owner_idx ON game.industry_jobs (owner_id, status);
CREATE INDEX IF NOT EXISTS industry_jobs_blueprint_idx ON game.industry_jobs (blueprint_id) WHERE status = 1;

-- Crimewatch (services/crimewatch.rs): -10 .. +10.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS security_status double precision NOT NULL DEFAULT 0;

-- Player ships lost; blob is the killBlob the kill report parses (attackers, items).
CREATE TABLE IF NOT EXISTS game.killmails (
    kill_id           bigserial PRIMARY KEY,
    kill_time         bigint NOT NULL,
    solar_system_id   bigint NOT NULL,
    victim_id         bigint NOT NULL,
    victim_corp_id    bigint NOT NULL,
    victim_ship_type  integer NOT NULL,
    damage_taken      double precision NOT NULL,
    final_char_id     bigint,
    final_corp_id     bigint,
    final_ship_type   integer NOT NULL,
    final_weapon_type integer NOT NULL,
    final_damage      double precision NOT NULL,
    final_sec_status  double precision NOT NULL,
    isk_lost          double precision NOT NULL,
    attacker_ids      bigint[] NOT NULL,
    blob              text NOT NULL
);
CREATE INDEX IF NOT EXISTS killmails_victim_idx ON game.killmails (victim_id, kill_id);
CREATE INDEX IF NOT EXISTS killmails_attackers_idx ON game.killmails USING gin (attacker_ids);

-- EVE mail (services/mail.rs): each message once, a status row per sender and recipient
-- (appConst.mailStatusMask* bits, mailLabel* bits plus custom labels from bit 16).
CREATE TABLE IF NOT EXISTS game.mail_messages (
    message_id             bigserial PRIMARY KEY,
    sender_id              bigint NOT NULL,
    to_character_ids       text NOT NULL DEFAULT '',
    to_list_id             bigint,
    to_corp_or_alliance_id bigint,
    title                  text NOT NULL,
    body                   text NOT NULL,
    sent_date              bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.mail_status (
    message_id  bigint NOT NULL REFERENCES game.mail_messages,
    char_id     bigint NOT NULL,
    status_mask integer NOT NULL DEFAULT 0,
    label_mask  integer NOT NULL DEFAULT 0,
    PRIMARY KEY (char_id, message_id)
);
CREATE TABLE IF NOT EXISTS game.mail_labels (
    char_id  bigint NOT NULL,
    label_id integer NOT NULL,
    name     text NOT NULL,
    color    integer,
    PRIMARY KEY (char_id, label_id)
);

-- Notifications; data is the YAML mapping the client's formatter for type_id reads.
CREATE TABLE IF NOT EXISTS game.notifications (
    notification_id bigserial PRIMARY KEY,
    receiver_id     bigint NOT NULL,
    type_id         integer NOT NULL,
    group_id        integer NOT NULL,
    sender_id       bigint NOT NULL,
    created         bigint NOT NULL,
    processed       boolean NOT NULL DEFAULT false,
    data            text NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS notifications_receiver_idx ON game.notifications (receiver_id, notification_id);

-- NPC (agent, corporation, faction) standings toward characters, -10 .. +10.
CREATE TABLE IF NOT EXISTS game.standings (
    char_id  bigint NOT NULL,
    from_id  bigint NOT NULL,
    standing double precision NOT NULL,
    PRIMARY KEY (char_id, from_id)
);

-- Player corporations (services/corps.rs); IDs from 98,000,000 (idCheckers.IsPlayerCorporation).
CREATE TABLE IF NOT EXISTS game.corporations (
    corporation_id bigserial PRIMARY KEY,
    name           text NOT NULL,
    ticker         text NOT NULL,
    description    text NOT NULL DEFAULT '',
    url            text NOT NULL DEFAULT '',
    tax_rate       double precision NOT NULL DEFAULT 0,
    lp_tax_rate    double precision NOT NULL DEFAULT 0,
    ceo_id         bigint,
    creator_id     bigint NOT NULL,
    station_id     bigint NOT NULL,
    is_recruiting  boolean NOT NULL DEFAULT true,
    friendly_fire  boolean NOT NULL DEFAULT false,
    shape1 integer, shape2 integer, shape3 integer,
    color1 integer, color2 integer, color3 integer,
    typeface       integer,
    welcome_mail   text NOT NULL DEFAULT '',
    created        bigint NOT NULL,
    deleted        boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX IF NOT EXISTS corporations_name_idx ON game.corporations (lower(name)) WHERE NOT deleted;
CREATE UNIQUE INDEX IF NOT EXISTS corporations_ticker_idx ON game.corporations (upper(ticker)) WHERE NOT deleted;
SELECT setval('game.corporations_corporation_id_seq', GREATEST((SELECT max(corporation_id) FROM game.corporations), 98000000), true);
-- A member's corporation roles (appConst.corpRole* mask), the same at every location.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS corp_roles bigint NOT NULL DEFAULT 0;

-- Applications and invitations (status: appConst.crpApplication*).
CREATE TABLE IF NOT EXISTS game.corp_applications (
    application_id   bigserial PRIMARY KEY,
    corporation_id   bigint NOT NULL,
    character_id     bigint NOT NULL,
    application_text text NOT NULL,
    status           integer NOT NULL,
    created          bigint NOT NULL,
    custom_message   text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS corp_applications_corp_idx ON game.corp_applications (corporation_id, status);
CREATE INDEX IF NOT EXISTS corp_applications_char_idx ON game.corp_applications (character_id, status);

-- Corporations a character has joined, and when.
CREATE TABLE IF NOT EXISTS game.employment_history (
    char_id        bigint NOT NULL,
    corporation_id bigint NOT NULL,
    start_date     bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS employment_history_char_idx ON game.employment_history (char_id, start_date);

-- Ship insurance contracts (services/insurance.rs): payout fraction of the insured value.
CREATE TABLE IF NOT EXISTS game.insurance (
    ship_id    bigint PRIMARY KEY,
    owner_id   bigint NOT NULL,
    type_id    integer NOT NULL,
    fraction   double precision NOT NULL,
    start_date bigint NOT NULL,
    end_date   bigint NOT NULL
);
CREATE INDEX IF NOT EXISTS insurance_owner_idx ON game.insurance (owner_id);

-- Jump clones (services/clones.rs): one per station per character.
CREATE TABLE IF NOT EXISTS game.jump_clones (
    clone_id    bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    location_id bigint NOT NULL,
    name        text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS jump_clones_owner_idx ON game.jump_clones (owner_id);
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS last_clone_jump bigint;

-- Personal bookmark folders, subfolders and bookmarks (services/bookmarks.rs).
CREATE TABLE IF NOT EXISTS game.bookmark_folders (
    folder_id   bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    name        text NOT NULL,
    description text NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS bookmark_folders_owner_idx ON game.bookmark_folders (owner_id);
CREATE TABLE IF NOT EXISTS game.bookmark_subfolders (
    subfolder_id bigserial PRIMARY KEY,
    folder_id    bigint NOT NULL,
    name         text NOT NULL
);
CREATE TABLE IF NOT EXISTS game.bookmarks (
    bookmark_id  bigserial PRIMARY KEY,
    folder_id    bigint NOT NULL,
    subfolder_id bigint,
    creator_id   bigint NOT NULL,
    item_id      bigint,
    type_id      integer,
    location_id  bigint NOT NULL,  -- solar system
    x double precision NOT NULL, y double precision NOT NULL, z double precision NOT NULL,
    memo         text NOT NULL,
    note         text NOT NULL DEFAULT '',
    created      bigint NOT NULL,
    expiry       bigint
);
CREATE INDEX IF NOT EXISTS bookmarks_folder_idx ON game.bookmarks (folder_id);

-- Calendar events (services/calendar.rs): owned by a character or a player corporation.
CREATE TABLE IF NOT EXISTS game.calendar_events (
    event_id    bigserial PRIMARY KEY,
    owner_id    bigint NOT NULL,
    creator_id  bigint NOT NULL,
    event_date  bigint NOT NULL,  -- FILETIME
    duration    integer NOT NULL DEFAULT 0,
    title       text NOT NULL,
    description text NOT NULL DEFAULT '',
    importance  integer NOT NULL DEFAULT 0,
    modified    bigint NOT NULL,
    deleted     boolean NOT NULL DEFAULT false
);
CREATE INDEX IF NOT EXISTS calendar_events_owner_idx ON game.calendar_events (owner_id, event_date);
CREATE TABLE IF NOT EXISTS game.calendar_responses (
    event_id bigint NOT NULL,
    char_id  bigint NOT NULL,
    status   integer NOT NULL,  -- appConst.eventResponse*
    PRIMARY KEY (event_id, char_id)
);
CREATE INDEX IF NOT EXISTS calendar_responses_char_idx ON game.calendar_responses (char_id);

-- Loyalty points per issuing NPC corporation.
CREATE TABLE IF NOT EXISTS game.lp_balances (
    char_id bigint NOT NULL,
    corp_id bigint NOT NULL,
    amount  bigint NOT NULL,
    PRIMARY KEY (char_id, corp_id)
);

-- Personal skill plans (public gateway eve_public.character.skill.plan).
CREATE TABLE IF NOT EXISTS game.skill_plans (
    plan_id      bytea PRIMARY KEY,         -- 16-byte UUID
    character_id bigint NOT NULL REFERENCES game.characters,
    name         text NOT NULL DEFAULT '',
    description  text NOT NULL DEFAULT '',
    -- Requirements in plan order: skill_ids[i] to levels[i].
    skill_ids    integer[] NOT NULL DEFAULT '{}',
    levels       integer[] NOT NULL DEFAULT '{}',
    active       boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS skill_plans_character_idx ON game.skill_plans (character_id);

-- Retail Serenity average market prices (crate::retail), loaded at startup.
CREATE TABLE IF NOT EXISTS game.retail_prices (
    type_id  integer PRIMARY KEY,
    average  double precision NOT NULL,
    adjusted double precision NOT NULL
);

-- New Eden Store: the account's PLEX vault (whole PLEX).
ALTER TABLE game.accounts ADD COLUMN IF NOT EXISTS plex_balance bigint NOT NULL DEFAULT 1000;

-- Redeeming queue: items owed to an account (store purchases, login rewards),
-- turned into items when claimed onto a character.
CREATE TABLE IF NOT EXISTS game.redeem_tokens (
    token_id    bigserial PRIMARY KEY,
    user_id     bigint NOT NULL REFERENCES game.accounts,
    type_id     integer NOT NULL,
    quantity    integer NOT NULL,
    label       text NOT NULL DEFAULT '',
    description text NOT NULL DEFAULT '',
    created     bigint NOT NULL  -- FILETIME
);
CREATE INDEX IF NOT EXISTS redeem_tokens_user_idx ON game.redeem_tokens (user_id);

-- Store purchases, for single-purchase and once-per-period offers.
CREATE TABLE IF NOT EXISTS game.store_purchases (
    user_id      bigint NOT NULL REFERENCES game.accounts,
    offer_id     integer NOT NULL,
    quantity     integer NOT NULL,
    price        bigint NOT NULL,
    purchased_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS store_purchases_user_idx ON game.store_purchases (user_id, offer_id);

-- Seasonal login reward claims: one row per account per reward day.
CREATE TABLE IF NOT EXISTS game.login_reward_claims (
    user_id     bigint NOT NULL REFERENCES game.accounts,
    campaign_id integer NOT NULL,
    day         date NOT NULL,      -- reward date (after the daily turnover)
    clone_grade integer NOT NULL,
    PRIMARY KEY (user_id, campaign_id, day)
);

-- Agent missions: one per character and agent (services/missions.rs). The site is a
-- deadspace point in system_id; its NPCs are spawned on first approach.
CREATE TABLE IF NOT EXISTS game.agent_missions (
    character_id bigint NOT NULL REFERENCES game.characters,
    agent_id     bigint NOT NULL,
    mission_id   integer NOT NULL,   -- sde_native.missions key (contentID)
    dungeon_id   integer NOT NULL,
    faction_id   bigint NOT NULL,
    state        integer NOT NULL,   -- appConst.agentMissionState*
    system_id    bigint NOT NULL,
    x            double precision NOT NULL,
    y            double precision NOT NULL,
    z            double precision NOT NULL,
    reward_isk   bigint NOT NULL,
    reward_lp    bigint NOT NULL,
    offered_at   bigint NOT NULL,    -- FILETIME
    accepted_at  bigint,
    PRIMARY KEY (character_id, agent_id)
);

-- Fitted modules' online state (dogma effectOnline), kept across sessions.
ALTER TABLE game.items ADD COLUMN IF NOT EXISTS online boolean NOT NULL DEFAULT false;

-- Saved ship fittings (charFittingMgr): fit_* arrays are fitData (typeID, flag, qty) rows.
CREATE TABLE IF NOT EXISTS game.fittings (
    fitting_id   bigserial PRIMARY KEY,
    owner_id     bigint NOT NULL,
    name         text NOT NULL DEFAULT '',
    description  text NOT NULL DEFAULT '',
    ship_type_id integer NOT NULL,
    fit_types    integer[] NOT NULL DEFAULT '{}',
    fit_flags    integer[] NOT NULL DEFAULT '{}',
    fit_qty      integer[] NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS fittings_owner_idx ON game.fittings (owner_id);

-- Charges loaded in fitted modules. The client keys them as the sublocation
-- (shipID, flagID, typeID), not as inventory items (services/combat.rs).
CREATE TABLE IF NOT EXISTS game.module_charges (
    ship_id  bigint NOT NULL,
    flag_id  integer NOT NULL,
    type_id  integer NOT NULL,
    quantity integer NOT NULL,
    PRIMARY KEY (ship_id, flag_id)
);

-- Weapon groups: each slave module fires with its master.
CREATE TABLE IF NOT EXISTS game.weapon_banks (
    ship_id   bigint NOT NULL,
    master_id bigint NOT NULL,
    slave_id  bigint PRIMARY KEY
);

-- Militias a player corporation's members may enlist in directly (fwCharacterEnlistmentMgr).
CREATE TABLE IF NOT EXISTS game.corp_enlistment_factions (
    corporation_id bigint NOT NULL,
    faction_id     bigint NOT NULL,
    PRIMARY KEY (corporation_id, faction_id)
);

-- Exploration (services/exploration.rs): cosmic anomalies and signatures per solar system,
-- wormhole connections (both ends share lifetime and mass), hackable site containers.
CREATE SEQUENCE IF NOT EXISTS game.exploration_site_seq START 9500000000000;
CREATE TABLE IF NOT EXISTS game.exploration_systems (
    system_id    bigint PRIMARY KEY,
    generated_at bigint NOT NULL
);
CREATE TABLE IF NOT EXISTS game.exploration_sites (
    site_id         bigint PRIMARY KEY,
    system_id       bigint NOT NULL,
    target          text NOT NULL,
    kind            smallint NOT NULL,          -- 0 anomaly, 1 signature
    strength_attr   bigint NOT NULL,
    ease            double precision NOT NULL,
    dungeon_id      bigint,
    name_id         bigint,
    archetype       bigint,
    faction         bigint,
    difficulty      bigint NOT NULL,
    x               double precision NOT NULL,
    y               double precision NOT NULL,
    z               double precision NOT NULL,
    overlay_x       double precision NOT NULL,
    overlay_y       double precision NOT NULL,
    overlay_z       double precision NOT NULL,
    deviation       double precision NOT NULL,
    in_space        boolean NOT NULL DEFAULT false,
    containers      bigint[],                   -- hackable containers (game.items) once spawned
    hacked          bigint[] NOT NULL DEFAULT '{}',
    wh_type         integer,
    wh_dest_system  bigint,
    wh_dest_class   bigint,
    wh_other        bigint,
    wh_born         bigint,
    wh_life_minutes double precision,
    wh_mass_max     double precision,
    wh_mass_used    double precision,
    wh_jump_mass    double precision,
    wh_nebula       bigint
);
CREATE INDEX IF NOT EXISTS exploration_sites_system ON game.exploration_sites (system_id);
-- Signatures a character has scanned down to a warpable result.
CREATE TABLE IF NOT EXISTS game.exploration_scanned (
    character_id bigint NOT NULL,
    site_id      bigint NOT NULL,
    PRIMARY KEY (character_id, site_id)
);

-- Mission content beyond kill missions (services/missions.rs): talk-to-agent, courier,
-- trade and agent-interaction steps, and the epic arc a mission belongs to.
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'kill';
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS arc_id bigint;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS dest_agent bigint;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS cargo_type integer;
ALTER TABLE game.agent_missions ADD COLUMN IF NOT EXISTS cargo_qty integer;
-- Epic arc progress per character: steps unlocked, accepted, completed or quit.
CREATE TABLE IF NOT EXISTS game.epic_arc_missions (
    character_id bigint NOT NULL,
    arc_id       bigint NOT NULL,
    mission_id   integer NOT NULL,
    state        text NOT NULL,      -- unlocked | accepted | completed | quit
    accepted_at  bigint,
    completed_at bigint,
    quit_at      bigint,
    PRIMARY KEY (character_id, arc_id, mission_id)
);

-- Character deletion (charUnboundMgr.PrepareCharacterForDelete): when the biomass queue
-- lets the character go (FILETIME); DeleteCharacter then marks it deleted.
ALTER TABLE game.characters ADD COLUMN IF NOT EXISTS delete_prepare_at bigint;

-- The AIR new player experience state per character (services/npe.rs):
-- storylines.common.airnpeconstants.AirNpeState (1 active, 2 completed, 3 skipped).
CREATE TABLE IF NOT EXISTS game.air_npe_state (
    character_id bigint PRIMARY KEY,
    state        smallint NOT NULL,
    changed_at   bigint NOT NULL
);

-- Mining (services/mining.rs): rocks of asteroid belts and of ore/ice/gas sites
-- (field_id: the belt's celestial ID or the exploration site ID), and belts' regrowth.
CREATE SEQUENCE IF NOT EXISTS game.asteroid_seq START 9700000000000;
CREATE TABLE IF NOT EXISTS game.asteroids (
    asteroid_id       bigint PRIMARY KEY DEFAULT nextval('game.asteroid_seq'),
    system_id         bigint NOT NULL,
    field_id          bigint NOT NULL,
    type_id           integer NOT NULL,
    quantity          bigint NOT NULL,
    original_quantity bigint NOT NULL,
    radius            real,
    x                 double precision NOT NULL,
    y                 double precision NOT NULL,
    z                 double precision NOT NULL
);
CREATE INDEX IF NOT EXISTS asteroids_system ON game.asteroids (system_id);
CREATE TABLE IF NOT EXISTS game.asteroid_fields (
    field_id    bigint PRIMARY KEY,
    system_id   bigint NOT NULL,
    grown_at    bigint NOT NULL,
    depleted_at bigint
);
