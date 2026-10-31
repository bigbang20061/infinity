# Lua extensions

The server loads `.lua` files from `lua.scripts_dir` at startup. The default is
`crates/server/lua`, relative to the server working directory. Set `[lua] enabled
= false` to disable Lua entirely. Only trusted operators should install scripts:
they can call the exposed game APIs. Each file gets its own global environment,
configuration table and persistent store. `require` only resolves text modules
inside the canonical scripts directory. Native libraries, `io`, `debug`, binary
chunks and unrestricted operating-system access are unavailable.

## Registration

```lua
register.rpc("exampleSvc", "GetAnswer", function(ctx, args, kwargs)
    return eve.int(42)
end)
register.command{
    name = "hello", help = "Example", gm = true,
    fn = function(ctx, args) return "ok" end,
}
register.event("BallparkBound", function(ctx) end)
```

RPC handlers receive the invocation context and typed tuple/dictionary wire
values. A `"*"` method matches unclaimed methods; returning Lua `nil` from a
wildcard falls through. Exact handlers must return an `eve` wire value. Command
arguments are Lua strings and commands return a Lua string. `gm = true` requires
GM privileges. `BallparkBound` is the only event currently supported. Duplicate
claims fail startup or reload rather than replacing another script.
Every claimed service/method is logged at load. Claims precede Rust handlers,
except retail cache and Macho control paths. A detected `fixed::root` overlap is
also logged as replacing a fixed answer; this does not enumerate all modeled
service overlaps.

## Wire values and context

Construct wire values with `eve.none`, `eve.int(n)`, `eve.float(n)`,
`eve.bool(b)`, `eve.str(s)`, `eve.unicode(s)`, `eve.tuple{...}`,
`eve.list{...}`, `eve.dict{key=value}`, `eve.keyval{key=value}`,
`eve.rowset(columns, rows)`, `eve.descriptor(columns)`,
`eve.crowset(descriptor, rows)`, `eve.instance(class, fields)`, or
`eve.reduce(callable, args, state)`. `str` is Python 2 bytes; use `unicode` for
player-facing text. Tuples and lists are distinct on the wire. Container members
must themselves be wire values, never raw Lua tables or scalars. Descriptor
columns are `{name, type}` pairs, for example `{{"itemID", "i8"}}`.
Incoming wire values support `:int()`, `:float()`, `:text()`, `:bool()`,
`:is_none()`, `:len()`, `:items()` and `:get(key)`.

`ctx.user_id`, `ctx.char_id`, `ctx.role`, `ctx:is_gm()` and
`ctx.session:get(key)` provide invocation data; session reads return a wire
value. `ctx:notify(event, ...)` takes wire arguments; `ctx:change_session(table)`
takes string keys and wire values. `ctx:bind(service, params)` returns a wire
callback. `ctx:user_error(name, dict)` raises a client error; its dictionary
values must be wire values. Context and session objects expire when the handler
finishes or is cancelled and must never be stored for later use.

## Persistence and configuration

`store.get(key)`, `store.set(key, value)`, `store.delete(key)` and
`store.keys(prefix)` operate only on the current script's keys. Values may be
booleans, finite numbers, UTF-8 strings, nested string-keyed tables or contiguous
1-based arrays. Nil is not a stored value. Mixed or sparse keys, cycles, functions,
userdata and non-finite numbers are rejected. An empty Lua table is stored as an
object. Nesting is limited to 64 levels; the encoded value cannot exceed
`lua.store_value_limit_bytes` (default 65536). Keys are returned in sorted order.
The table is server-owned; scripts cannot issue SQL. Reload retains stored values.
`store.set` and `store.delete` are single database statements. If a call times
out while one is awaiting the database, it may still have committed; a timeout
does not promise rollback.
`config` holds the script's own `[lua.scripts.<filename>]` TOML settings.

`messages.motd_help`, `messages.motd_saved` and `messages.motd_empty` are the
shipped MOTD messages. Player-facing text from custom scripts should follow the
server's localization policy.

## Other host functions

`log.info`, `log.warn` and `log.debug` take a string and attribute it to the
script. `time.now_filetime()` returns Windows FILETIME; `time.now_ms()` returns
wall-clock milliseconds; `time.now_tick(system)` reads the running system actor's
destiny clock and errors if the actor is not running. `sde.type(id)`,
`sde.attribute(type_id, attribute_id)`, `sde.group(id)`, `sde.category(id)` and
`sde.system(id)` read startup data; missing records return nil. `world.ball(system,
id)` returns a copied snapshot or nil; `world.notify_pilot(character, event,
wire_args)` sends a typed notification.
The type snapshot contains `type_id`, `group_id`, `category_id` and `name`;
group contains `category_id` and `name`, category contains `name`, and system
contains `solar_system_id`, `name` and `constellation_id`. Attribute returns
a number or nil. A ball snapshot contains `id`, `type_id`, `owner_id`,
three-element `position` and `velocity` arrays, and `radius`.

`world.schedule(system, delay_ms, op, args)` asynchronously enqueues a Rust
actor timer and returns whether the running actor accepted it; a missing or
stopped system returns false and is not loaded. Acceptance does not mean the
operation has completed. Supported operations are:

- `notify`: `{character=id, event=name, args={wire_values...}}`, with a
  contiguous typed wire-value sequence.
- `remove_ball`: `{id=ball_id}`.
- `spawn_npc`: `{type_id=id, corp=id, position={x,y,z}, aggro_target=id_or_nil}`.
  The type comes from the SDE and the coordinates must be finite. Spawning is
  deferred outside the actor; failures are logged asynchronously.

`world.ball` samples the current interpolated position between destiny ticks.

## Game data

Reads are asynchronous: `dao.wallets.balance(character)`,
`dao.items.by_owner(owner)`, `dao.items.by_location(location)`,
`dao.characters.get(character)`, `.name(character)`, `.ship(character)`,
`dao.skills.levels(character)`, `dao.corps.of(character)`, and
`dao.standings.get(character, from)`. Item reads return arrays of tables with
`item_id`, `type_id`, `owner_id`, `location_id`, `flag_id`, `quantity`,
`singleton`, `group_id`, `category_id`, and optional `custom_info`.
Character `get` returns nil when absent or a table with `name`,
`corporation_id`, `race_id`, `bloodline_id`, `ancestry_id`, `school_id`,
`gender_id` and `created`; `name`, `ship`, `corps.of` and `standings.get`
return nil when absent. `skills.levels` returns a type-ID-to-level table.
Wallet balance is a number. Item-move quantities must also fit a signed
32-bit inventory quantity.

Mutators require the live invocation context as their first argument:

- `dao.wallets.credit(ctx, character, amount)` and
  `dao.wallets.debit(ctx, character, amount)` return nil.
- `dao.items.create(ctx, owner, location, flag, type_id, quantity)` creates or
  merges a stack and returns nil.
- `dao.items.move(ctx, item, container_item, flag_or_nil, quantity_or_nil)`
  returns the destination item ID or nil.
- `dao.items.delete(ctx, item)` applies the caller's existing trash ownership
  and active-ship policy and returns nil.
- `dao.mail.send(ctx, recipient_ids, title, body, list_or_nil, corp_or_nil,
  reply_to_or_nil, forwarded_from_or_nil)` returns the message ID. Mailing
  lists remain unsupported; corporation mail follows the native service policy.
- `dao.standings.set(ctx, character, from, value)` returns nil; standings must
  be finite and between -10 and 10.

IDs and quantities must be positive and amounts finite and positive. Mutations
use the server's service operations and transaction/outbox rules. Once a
multi-step mutation has been dequeued, the execution deadline cannot interrupt
its transaction and post-commit effects: it finishes before a timeout is
reported, and the script is not resumed after its deadline.

## Limits and administration

Defaults are 64 MiB VM memory, 50 ms and 5,000,000 instructions per call, with
an instruction hook every 1000 instructions. Configure these with
`memory_limit_mb`, `call_budget_ms`, `call_budget_instructions` and
`instruction_hook_interval` under `[lua]`. `/lua list` shows claims and limits;
`/lua reload` builds a fresh registry and swaps it only on success. In-flight
handlers retain their old registry; Lua globals reset, but stored values survive.
The shipped `/motd` command is GM-only: `/motd set <text>` saves a message and
`/motd` reads it. The message is sent on `BallparkBound`.

The bundled `fixed_answers.lua` claims five reference methods. The client reads
the information-gathering keyval when that service starts. Character rename
details are requested only when the rename feature is enabled; localization
QA-wrap requires the QA role, and text changes require an unpackaged client
with localization hashes. The anti-cheat canary query requires its feature
switch. These conditional claims are not evidence that five calls occur in
every login or docking walkthrough.

Test scripts against representative typed arguments, malformed inputs and resource
limits, and run the release tests. The persistent store database fixture requires a
separate initialized PostgreSQL database; see its test for the explicit environment
variable. Validate player-visible behavior with the real client.
