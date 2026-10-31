# Infinity

A server emulator for the EVE Online **Infinity** client (build 3653293, machoNet 496), written in Rust.

A single process acts as both proxy and sol node. It also serves XMPP chat, the image server, the public gRPC gateway and the content CDN. Game state is stored in PostgreSQL.

> This project is not affiliated with or endorsed by CCP Games. EVE Online is a trademark of CCP hf.

## Features

- **Login and characters.** GPS handshake with the Placebo crypto pack. An account is created the first time a new user name logs in. Character selection and creation. Every account is treated as Omega.
- **Stations and items.** Inventory and hangars, and the assets window with asset search. Boarding, assembling and repackaging ships. Saved fittings, ship insurance, jump clones and reprocessing. Private notes in the info window.
- **Dogma.** Effective attributes are computed on the server from skills, hull, modules and charges, including stacking penalties. Passive skill bonuses are also sent to the client as its character brain, so the fitting window and the HUD show them; the brain is sent again when finished skill training is recorded.
- **Space.** Undock and dock, approach, align, orbit and follow, warp, and stargate jumps. Each solar system runs as its own actor, so systems advance independently and never block each other. Ships move on the client's one-second destiny tick: a command is stamped with the tick the system is on and takes effect before the next step, as on the client, and flight toward a distant point (go-to and align) uses carbonengine destiny's integration, so other pilots see the same course. Timers and NPCs run every 100 ms and read positions interpolated between ticks, as the client draws them. A per-system pilot cap with a jump queue is optional.
- **Combat.**
  - Targeting, ammunition, weapon groups, turrets and missiles, and capacitor.
  - NPC AI, bounties, wrecks and loot.
  - Crimewatch: timers, criminal flags, security status, CONCORD and killmails.
- **Skills.** Training queue. Skill plans are served through the public gateway.
- **Economy.**
  - Regional market: seeded NPC orders, escrow, taxes and fees, history.
  - Contracts: item exchange, auction and courier.
  - Industry: manufacturing, research, copying and invention.
- **Agents.** Kill missions, epic arcs, NPC standings, loyalty points and LP stores.
- **Exploration.** Anomalies and signatures, probe and directional scanning, wormholes, the data and relic hacking minigame, and belt rats.
- **Mining.** Asteroid belts, ore, ice and gas, mining modules, and belt regrowth.
- **Social.** Chat (local, corporation, fleet and other channels), fleets, EVE mail and notifications, the calendar, and bookmarks.
- **Corporations and alliances.**
  - Player corporations: founding, applications and invitations, roles and titles, a bounty tax paid into the corporation wallet, and bulletins, contacts and labels.
  - Seven wallet divisions with ISK transfers between characters and corporations. Offices rented per station, with corporation hangars and rent bills that can be paid automatically.
  - Alliances: founding, applications, member removal, executor support, and bulletins and contacts.
- **Store.** New Eden Store catalogue, PLEX vault, redeeming queue and seasonal login rewards.
- **GM slash commands** (see [GM commands](#gm-commands)).

## Known limitations

- The new player tutorial (AIR NPE) cannot run. Its dungeon layouts and behaviour trees never shipped with the client, so new characters start in their school's station.
- Not supported:
  - wars, sovereignty, and alliance capital systems and prime time
  - corporation shares and wallet journals
  - corporation contracts and corporation industry jobs
  - fleet warps, boosts and the fleet finder
  - shared bookmark folders
  - implants
- Some runtime state lives only in memory and is lost when the server restarts:
  - fleets, combat and Crimewatch timers, ship damage, and in-progress hacking;
  - everything spawned into space (NPCs, wrecks, scheduled effects);
  - a ship's velocity, warp and cloak, so a restarted ship starts at rest.
- A reload still running when the server stops is not resumed. At the next start its charges go back to the ship's cargo hold.
- A ship's position in space is saved when its pilot leaves the game, and every 30 seconds while it moves. A crash therefore loses at most the last 30 seconds of movement.
- Stopping the server with Ctrl-C or SIGTERM (`docker compose stop`) saves the position of every ship in space before it exits. The server waits at most 10 seconds for these writes.
- Space movement matches carbonengine destiny only for flight toward a distant point (go-to and align) by ships of ordinary mass. The slow-down on arrival at a near point, approach, orbit, follow, stop, warp alignment and warp flight are approximations, and there are no collisions, so ships do not bump off stations, gates or each other. After an undock bump, an orbit or a warp, other pilots can see a ship somewhere other than where its pilot does, until the next full state update.
- Balances are stored with up to 36 digits (ISK: 34 integer digits and 2 decimal places; PLEX: 36 integer digits), and the server works with the full value. The client is sent at most the largest balance it displays without scientific notation: 1,000,000,000,000,000,000 ISK for a character wallet, 9,223,372,036,854,774,784 ISK for a corporation wallet, and 99,999,999,999,999 PLEX. ISK reaches the client as a double, so above about 45 trillion ISK it can no longer show every cent.

## Repository layout

| Path | Contents |
|---|---|
| `crates/server` | The server (`infinity-server` binary) |
| `crates/server/src/services` | Remote services the client calls (`sm.RemoteSvc(name).Method(...)`), one module per feature area |
| `crates/server/sql` | `game_schema.sql` (applied at every start) and `market_seed.sql` |
| `crates/server/retail` | Data captured from the retail server for this build (CDN content, cached machoNet calls) |
| `crates/marshal` | `blue.marshal` codec, plus the `marshal-dump` tool |
| `crates/sde-import` | Loads static data bundles (JSONL) into PostgreSQL |
| `crates/blue-patch` | Patches the client's `blue.dll` so an edited `start.ini` still loads |
| `db/` | Per-table database dumps that are loaded into a fresh database |
| `scripts/` | Database init and dump, client patching and launching, and the dev loop |

## Requirements

- Docker with Compose.
- The Infinity client, build 3653293, on Windows.
- PowerShell, to run the helper scripts in `scripts/`.
- `crates/server/proto/eve_protos.desc`: the protobuf descriptor set extracted from the client.
  - The public gateway decodes the client's messages with it, and the server does not compile without it.
  - It is included in the repository. The extraction tool is not.
- Optional: a Rust toolchain, for local builds and tests. A local toolchain also lets `patch-client.ps1` build `blue-patch` itself instead of running it from the Docker image.

## Quick start

1. Start the database and the server:

   ```sh
   docker compose up -d --build
   ```

   On the first start with an empty database volume, `scripts/db-init.sh` loads every dump under `db/`. This takes a few minutes, and the server waits until the database is healthy.
2. Prepare the client. Close the client first, and run this again after every launcher update:

   ```powershell
   scripts/patch-client.ps1 -Root <client folder>
   ```

   The script does two things:
   - It patches `bin64/blue.dll` so the client skips its manifest entry hash check. The original is kept as `blue.dll.orig`.
   - It sets `cryptoPack = Placebo` in `start.ini`. The original is kept as `start.ini.orig`.

   Pass `-Check` to only report what is needed.

   `-Root` is remembered in `.client-root` (ignored by git), so later runs of both scripts can omit it. Without it, the scripts use `$env:INFINITY_CLIENT`, then the remembered folder, then `<drive>:\EVE\SharedCache\infinity` on any drive.
3. Launch the client against the server:

   ```powershell
   scripts/run-client.ps1 -User <name> -Password <password>
   ```

   The script does the following:
   - It disables TLS for chat in the client's `prefs.ini`.
   - It points the client at the public gateway.
   - It starts `portrait-sync.ps1`, which uploads portraits the client renders locally to the image server.

   Pass `-AutoSelect <slot or characterID>` to enter the game straight away. The default server address is `127.0.0.1`; pass `-ServerHost` to change it.

## Ports

| Port | Service |
|---|---|
| 26000 | machoNet (game client) |
| 5222 | Chat (XMPP, plain TCP) |
| 26001 | Image server (portraits, logos, type images; `PUT` stores a portrait) |
| 26002 | Public gateway (gRPC over plaintext HTTP/2) |
| 80 | Content CDN (the client always fetches it on port 80) |

## Configuration

The server reads its configuration from environment variables. `docker-compose.yml` sets `DATABASE_URL`, `RUST_LOG` and `SYSTEM_CAPACITY`.

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | (required) | PostgreSQL connection string |
| `DATABASE_POOL_SIZE` | `16` | Maximum number of PostgreSQL connections the server opens. Keep it below the server's `max_connections` (PostgreSQL's default is 100), leaving room for tools. |
| `PUBLIC_HOST` | `127.0.0.1` | Address clients use to reach this host (image server and CDN URLs). Set it when clients are not local. |
| `SYSTEM_CAPACITY` | `0` | Pilots in space per solar system before stargate jumps queue (`0` = no limit) |
| `BIND` | `0.0.0.0:26000` | machoNet listener |
| `CHAT_BIND` / `CHAT_DOMAIN` | `0.0.0.0:5222` / `infinity` | XMPP listener and domain |
| `IMAGE_BIND` / `IMAGE_PORT` / `IMAGE_DIR` | `0.0.0.0:26001` / `26001` / `/data/images` | Image server listener, the port advertised to clients, and its storage |
| `IMAGE_UPSTREAM` | `https://images.evetech.net` | Where missing NPC, NPC corporation and type images are fetched from once (empty = never) |
| `GATEWAY_BIND` | `0.0.0.0:26002` | Public gateway listener |
| `CDN_BIND` | `0.0.0.0:80` | Content CDN listener |
| `MACHO_VERSION`, `BOOT_VERSION`, `BOOT_BUILD`, `BOOT_CODENAME`, `BOOT_REGION`, `NODE_ID` | match build 3653293 | Values the client checks during the handshake |
| `RUST_LOG` | `info,infinity_server=debug` | Log filter |

## Database

The database has four schemas:

- `sde` holds the typed, indexed static data tables.
- `sde_native` and `sde_compat` hold the raw static data bundles, one `jsonb` table per file.
- `game` holds everything players create: accounts, characters, items, the market, mail and so on.

Lifecycle:

- **First start.** `scripts/db-init.sh` loads `db/<schema>/<table>.sql.gz` into an empty volume. Tables whose foreign keys aren't satisfied yet are retried until everything is loaded.
- **Every server start.** The server applies `crates/server/sql/game_schema.sql`, loads the captured retail prices, and seeds the NPC market if it is empty. It then reads the static data it uses from `sde`, `sde_native` and `sde_compat` into memory once; after startup the server never queries those schemas, so static data changes take effect at the next start.
- **Reset.** Remove the database volume to start over (`docker compose down`, then `docker volume rm infinity_pgdata`). The next `docker compose up` imports the dumps again.
- **Rebuild the static data.** Put the static data JSONL bundles (`eve-online-client-static-data-<build>-jsonl` and `eve-online-static-data-<build>-jsonl`) under `./sde/`, run `docker compose run --rm sde-import`, then restart the server.
- **Refresh the dumps.** `scripts/db-dump.ps1` replaces the folders under `db/` with dumps of the running database.

## Development

- **Builds.** Only release builds are used: `cargo build --release`, `cargo test --release`, or `docker build --target test .` to run the tests in Docker. The toolchain version is pinned in `rust-toolchain.toml`.
- **CI.** Every pull request to `main` must pass the `CI` workflow before it can merge:
  - `test-linux`: build and tests on Linux.
  - `build-windows`: build on Windows.
  - Both treat compiler warnings as errors.
  - Both build only when the pull request changes code: `crates/`, the Cargo files, `rust-toolchain.toml` or the workflows. For a pull request that changes no code, they report as skipped and it can still merge.
- **Dev loop.** `scripts/iterate.ps1` does the following:
  - rebuilds and restarts the server container;
  - launches the client into the game;
  - waits (`-Seconds`, default 70);
  - prints the run's unhandled calls, server errors and client exceptions.

  `-NoBuild` skips the rebuild.
- **Tools.**
  - `marshal-dump [--check] <file>...` decodes `blue.marshal` files. `--check` re-encodes them and reports only the status.
  - `blue-patch [--check] <bin64/blue.dll>` is the patcher used by `patch-client.ps1`.
- **Contributing.** Contributor and coding-agent guidelines are in [AGENT.md](AGENT.md).

### GM commands

Slash commands need GM roles on the account. Grant them in the database, then log in again:

```sql
UPDATE game.accounts SET role = role | 27021597764489216 WHERE lower(user_name) = lower('<name>');
```

| Command | Effect |
|---|---|
| `/tr me <itemID\|characterID\|solarSystemID\|stationID\|celestialID>` | Teleport |
| `/wh` | Teleport next to a wormhole (scanned down for you) |
| `/scanall` | Scan every signature in the current system to 100% |
| `/sites` | List the current system's anomalies and signatures |
| `/create <typeID\|name> [qty]` (alias `/createitem`) | Create items in the hangar (docked) or cargo |
| `/setisk <amount>` | Set your character's ISK wallet balance |
| `/addisk <amount>` | Add ISK to your character's wallet |
| `/setplex <count>` | Set your account's PLEX vault balance |
| `/addplex <count>` | Add PLEX to your account's vault |
| `/heal` | Repair the ship in space |
| `/help` | List the commands |

## Releases

Tagged versions are published on the repository's GitHub releases page. Each release contains:

- `infinity-<version>-x86_64-linux.tar.gz`
- `infinity-<version>-x86_64-windows.zip`
- `SHA256SUMS`

Each package holds `infinity-server`, `sde-import`, `marshal-dump`, `blue-patch` and this README.

To release, a maintainer pushes a tag `vMAJOR.MINOR.PATCH` that points at a commit on `main`, for example `git tag v0.1.0 && git push origin v0.1.0`. Tags with any other name do not start a release.

## License

The workspace manifest declares the MIT license.
