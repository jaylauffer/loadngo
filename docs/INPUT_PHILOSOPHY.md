# Input Philosophy

## Status and purpose

Status: **standalone values doc, anchoring existing threads — no new mechanism implemented here.** Written 2026-09-05 alongside [GAMEPAD_INPUT.md](GAMEPAD_INPUT.md), which is the first concrete design this doc informs. This is not a new invention — it makes explicit a value that already shapes scattered parts of `loadngo` (see below), so that future input-surface design (gamepad now, potentially others later) has something real to check itself against instead of re-deriving values ad hoc per feature.

## The throughline: energy as the currency of exchange

A player exchanges something real and finite with a game every time they play it: attention, time, and physical or mental effort. That exchange is worth taking seriously in the same way `ARCHITECTURE.md`'s guiding philosophy already does for a different kind of exchange — "save as many lives as we may, even our own," with concrete expansions like "preserve meaningful work and state," "avoid needless data loss," and "treat storage and lineage as ways of protecting lived effort, not merely moving bytes." Those lines are about protecting what a person already put in. This doc names the adjacent, not-yet-explicit half of the same idea: what a game asks a player to put in, in the first place, deserves the same respect — energy spent playing is not a resource `loadngo` or the games built on it should be careless with.

This is one more expression of that root philosophy, not a competing one. Nothing here overrides `ARCHITECTURE.md`; it extends the same throughline into the input/engagement side of the engine rather than only the storage/persistence side it was originally written for.

## Existing evidence this value already shapes loadngo

This isn't a new value being introduced from nothing — it's already load-bearing in three places, just never stated as a single principle:

- `sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md` states directly that "avoiding wasted compute/energy is part of `loadngo`'s stated intent to build toward a better world, not just a cost-efficiency nicety." That's about compute energy, not player energy, but it establishes that "energy" is already a value-laden word in this project's own vocabulary, not just a technical resource to optimize.
- `loadngo/docs/WORKER_FIRST_TASK_MODEL.md` and `skills/loadngo-worker/SKILL.md` both treat a worker node's "energy budget" as a first-class thing to weigh, alongside capability and time, before committing to work — energy-as-a-resource-to-spend-carefully, in the task/worker economy rather than gameplay, but the same underlying respect for a finite resource.
- `HostFrame.foreground` already pauses simulation and audio when the app isn't in the foreground, rather than continuing to run (and make noise) off-screen. This is the engine's own existing precedent for respecting the player's real-world state — not a hypothetical this doc invents, but a real, shipped behavior already doing a version of what "perform activities in known healthy ways" asks for more broadly.

## Perform activities in known healthy ways

Stated directly, as a value: `loadngo` and the games built on it should not be designed in ways that reward or demand unhealthy patterns of engagement from a player — punishing rest, demanding sustained high-intensity input beyond comfortable limits, or otherwise treating a player's continuous attention as something to be extracted rather than exchanged. This is a value statement about design taste, not a mechanism. Nothing in this doc proposes a system that measures, scores, or enforces "healthy" play — that would be a much larger and more sensitive undertaking than this doc is scoped for, and isn't decided or wanted here.

## Forward compatibility with physical-energy input (non-goal now)

The framing above — energy as something exchanged between player and game — points at a category of future input this philosophy anticipates but does not build: wearables, motion sensors, or other devices that read a player's real physical effort as a signal a game responds to. Naming this now is useful precisely because it lets today's nearer-term work (a physical gamepad abstraction) make one small, justified choice that keeps the door open, without scope-creeping into anything wearable-shaped. Explicitly: no wearable or biometric hardware support, no energy/effort scoring system, and no health-tracking mechanic are being planned, scheduled, or implied by this doc.

## Concrete design consequence for gamepad input today

The one real consequence this philosophy has on current work: prefer continuous/analog representations over booleans wherever a signal is naturally continuous, rather than collapsing it into a threshold-crossing boolean. A future effort- or energy-based input (a heart-rate band, a grip-pressure sensor, anything of that shape) would almost certainly arrive as a continuous 0..1 value, not a boolean — so keeping today's analog signals genuinely analog in the type system, instead of flattening them into digital buttons for convenience, is "aligning design with the principle" in a small, checkable way, without building any of the future thing itself.

This lands concretely in [GAMEPAD_INPUT.md](GAMEPAD_INPUT.md)'s type scaffold: `GamepadStick` and `GamepadTrigger` are raw `f32`/`PointF` values, never duplicated as boolean buttons crossing an internal threshold, and deadzone shaping is a caller-side helper applied on top of the raw analog value rather than baked into storage — so the raw, continuous signal is always the thing actually kept.

## Explicit non-goals

- No wearable or biometric hardware support of any kind.
- No energy/effort scoring system, meter, or mechanic.
- No health-tracking feature.
- No change to any existing input type beyond what `GAMEPAD_INPUT.md` already scaffolds.

## Related docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — the root guiding philosophy this doc extends; cross-linked back from there too.
- [GAMEPAD_INPUT.md](GAMEPAD_INPUT.md) — where the one concrete design consequence (analog over boolean) actually lands in code.
- [WORKER_FIRST_TASK_MODEL.md](WORKER_FIRST_TASK_MODEL.md) and [`skills/loadngo-worker/SKILL.md`](../skills/loadngo-worker/SKILL.md) — the existing "energy budget" precedent in the task/worker economy, cited above.
- [`../../sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md`](../../sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md) — the explicit "avoiding wasted compute/energy... build toward a better world" line quoted above.
