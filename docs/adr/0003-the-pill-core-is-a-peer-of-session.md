---
status: accepted, not yet implemented (see #39)
---

# The Pill core is a peer of Session, not downstream of it

Today `Session` authors the pill: it emits `SetPill` and `DismissPill`, and owns the terminal flash's linger in its own `Phase`. That works only while the pill's life *is* the session's life. The resident pill breaks that assumption — the pill will exist when no session does. So the pill gets its own pure core, peer to `Session`, holding **Presence** × **Activity** and deriving the **pill mode** itself; `Session` becomes one of its drivers rather than its author.

## Why not keep Session driving the pill

Because the rules that matter are the ones that only appear once the pill outlives the session, and they would all land in the winit adapter:

- a fullscreen app taking focus while a chord is pressed,
- hover arriving during a session,
- residency toggled off mid-flash.

Rules in the adapter are unreachable by tests. Everything below this layer — `Session`, and the activation FSM below it — is already a pure core taking `now: Instant` and returning `Command`s precisely so these races are assertable. Leaving the pill's rules in the adapter would be the one place the pattern stops, and it would stop exactly where the concurrency is hardest.

**Activity outranks presence.** A session is always visible, even behind a fullscreen app, and hover can never expand a pill that is recording. The state after a session ends is *derived* from presence, never remembered — a remembered return-state is wrong the moment presence changed while the session ran.

## Consequences

- `Session` no longer emits `SetPill` or `DismissPill`; both variants disappear. It reports activity, and the event loop feeds that to the Pill core.
- `Phase::Done`, `SUCCESS_LINGER` and `ERROR_LINGER` move out of `Session` and into the Pill core, which means **a session now ends the moment its outcome is known** — the terminal flash outlives it.
- The pill adapter performs commands and derives frames. It holds no lifecycle rules.
- `Tail` is retired as a term. It was renamed to `Phase` when `Session` was deepened; this decision removes the concept from `Session` altogether.

## Status

Specified in [#39](https://github.com/Reuzehagel/draft-v2/issues/39) and not yet built — `Session` still emits `SetPill` today. `CONTEXT.md` carries the vocabulary ahead of the code. The ticket ships the seam with presence pinned to `Off`, so behaviour on screen is unchanged until residency itself lands.
