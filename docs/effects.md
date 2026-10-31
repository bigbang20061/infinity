# Effects

## Architecture and activation

The single registry in `crates/server/src/services/effects/registry.rs` resolves an
SDE effect ID to a behaviour kind and a descriptor carrying the effect's SDE
metadata, including its GUID, category, timing and range attribute IDs, and
behaviour flags. Explicit behaviour classifications and exclusions take
precedence over the generic modifier fallback. Effect names may be used to find
SDE data, but do not select runtime behaviour.

Activation checks that the item belongs to the pilot, is fitted to the active
ship, and is online where required. The shared validation applies at the
activation trust boundary; the online RPC and activation of the online effect
use one online operation. Scope is checked against the item's type and group:
explicitly excluded groups are denied, rather than admitting only a client-side
allowlist. A recognised kind is not necessarily an implemented behaviour.
Unsupported or out-of-scope activations are visibly refused, including those
with modifier data; they are never treated as a generic animation.

Unsupported attempts are logged and recorded once per effect name and type ID
per process. A GM with the existing slash role can use `/effects unsupported`
to list recorded pairs. The process-local list resets on restart and is not an
inventory of every unsupported SDE effect.

## Behaviour status

| Kind or family | Status |
|---|---|
| Turret, missile, local shield/armour/hull repair, propulsion, mining | Existing behaviour uses the shared lifecycle. |
| Modifier-only and online effects | Supported through the modifier and online paths, subject to validation and scope. |
| Hacking and probe launch | Classified for existing exploration paths; classification alone does not guarantee every item or call path. |
| Projected effects, ECM, capacitor warfare and transfer, fueled and remote repair | Known later families, visibly refused until their behaviour is implemented. |
| Smartbomb, Vorton, cloak, micro jump drive, salvage, tractor, scans, target assistance and bombs | Known later families, visibly refused until implemented. |
| Explicitly excluded capital, industrial, structure and other out-of-scope groups | Refused even when an effect has modifiers. |

## Module map

All paths below are under `crates/server/src/`.

- `services/effects/registry.rs`: numeric classification, scope and resolved
  effect metadata; `mod.rs` defines the kinds.
- `services/effects/lifecycle.rs`: activation, cycles, repeat and stopping;
  `weapons.rs`, `repair.rs`, `propulsion.rs`, `mining.rs` and `hacking.rs`
  contain the existing family-specific work.
- `services/effects/environment.rs` and `notify.rs`: effect environment and
  notification payloads; `unsupported.rs` owns refusal diagnostics.
- `services/combat/` retains targeting, ammunition, health, damage and NPC
  state. `services/fit.rs` evaluates attributes. Space actor timers schedule
  cycle work.
- `sde/dogma.rs` loads dogma data; `sde/dogma/python_effects.rs` supplements
  Python-only modifiers.

## Wire contract

`OnGodmaShipEffect` carries eleven arguments: `(itemID, effectID, time, start,
active, environment, startTime, duration, repeat, error, actualStopTime)`.
The environment is the eight-element list `[itemID, charID, shipID, targetID,
otherID, [], effectID, structureID]`. A loaded charge or script is identified
by its client sublocation tuple `(shipID, flagID, typeID)` in `otherID`, not
by the database item ID; absent values are `None`.

`OnModuleAttributeChanges` contains entries `(OnModuleAttributeChange, owner,
item, attribute, time, new, old, None)`; a charge quantity change uses the
same sublocation tuple as its item. `OnSpecialFX` carries fourteen arguments:
`(shipID, moduleID, moduleTypeID, targetID, chargeTypeID, guid, isOffensive,
start, active, duration, repeat, startTime, graphicInfo, area)`.

## Fit evaluator data and scope

Fit evaluates character, ship, module, charge and skill nodes alongside active
brain items, drones and explicitly supplied incoming projections. Brain items
are limited to character-owned implants in flag 89 and boosters in flag 88;
hangar and cargo items do not apply. Player implant plugging and booster use
RPCs, including booster lifecycle, remain with skills-complete and are not
implemented here.
Character item info transmits base character attributes because the client
applies the compiled brain afterward. Effective character and training
attributes come from the same Fit evaluator, so implant bonuses are applied
once rather than doubled.

Modifier evaluation covers item, location, group and required-skill domains,
including character-location brain items and drones. Operation 8 applies
unpenalized percent modifiers; special operation 9 derives skill level from
skill points and rank. Overload modifier data is evaluated only when the
snapshot marks a node overloaded; this does not add an overload RPC.
Ordinary numeric evaluation retains per-fit memoization, not a persistent
keyed cache with invalidation. Client stacking penalties independently truncate
the strongest upward and downward modifiers to eight each.

`Fit::load_projected` is the single loading implementation and accepts
source-effective values for a specific incoming effect, its source module and
type, and optional charge or script type. Only that effect's target modifiers
reach the victim; the evaluator does not load the source ship. `Fit::load` is
an unprojected convenience. PR3 owns runtime projection storage and expiry and
must migrate all in-space callers to provide system projections. This evaluator
support does not implement runtime electronic warfare, drone gameplay or new
RPCs.

## SDE and Python modifiers

At startup, dogma loads SDE effects and modifier rows. The Python modifier
table matches the actual SDE effect names at load time, for example
`remoteWebifierFalloff`, `remoteTargetPaintFalloff`, `remoteSensorDampFalloff`
and `remoteSensorBoostFalloff`. Its rows supplement an effect only when the SDE
modifier list is empty; they are never combined with a non-empty list. The
result follows the normal indexed SDE/Fit evaluation path. Modifier data for
a later family does not implement or enable that family's activation.

## Adding a family

1. Identify its SDE effect IDs, item groups, metadata and client behaviour. Add
   named IDs and classification in the registry; maintain explicit exclusions
   until the corresponding behaviour is actually supported. Do not dispatch by
   effect-name strings or use the generic modifier path to bypass a refusal.
2. Implement the behaviour under `services/effects/` and integrate it with the
   shared lifecycle, validation, resolved metadata and exhaustive kind matches.
   Read effective attributes through Fit, schedule space work on the actor, and
   use the shared notification builders.
3. Cover registry resolution, scope/refusal boundaries and observable cycle and
   wire behaviour in the existing separate test files. Update this document
   when support changes. Protocol and gameplay changes also need real-client
   verification.
