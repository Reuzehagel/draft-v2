---
status: accepted
---

# The Pill core is a peer of Session, not downstream of it

`Session` used to author the pill: it emitted `SetPill` and `DismissPill`, and owned the terminal flash's linger in its own `Phase`. That works only while the pill's life *is* the session's life. The resident pill breaks that assumption — the pill will exist when no session does. So the pill gets its own pure core, peer to `Session`, holding **Presence** × **Activity** and deriving the **pill mode** itself; `Session` becomes one of its drivers rather than its author.

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

Built in [#39](https://github.com/Reuzehagel/draft-v2/issues/39) as `src/pill/core.rs`.

[#42](https://github.com/Reuzehagel/draft-v2/issues/42) added the residency toggle as the core's second driver, [#44](https://github.com/Reuzehagel/draft-v2/issues/44) the hover poller, and [#45](https://github.com/Reuzehagel/draft-v2/issues/45) the fullscreen watcher. All three of `Presence`'s variants are now driven in the shipped app, and both rules that were asserted against the command list ahead of their drivers held unchanged when the drivers arrived: a chord press during suppression shows the whole session and returns to hidden when the flash retires, and hover cannot expand a recording pill.

Three drivers of one axis turned out to want one more rule, and it belongs to the adapter rather than the core: **presence is composed, never pushed.** Each driver owns a fact — residency, suppression, hover — and one function turns the three into the single `Presence` the core is handed. Three writers of `set_presence`, each knowing only its own fact, would overwrite the other two; the hover poll runs every loop, so it would have taken the pill straight back out over the fullscreen app the watcher had just hidden it from. The core is unchanged by this: it still takes one value, and *activity outranks presence* still decides the rest.

The decision has held under its first real test. Residency toggled off mid-session changes nothing on screen until the flash retires, and toggled on mid-session sends the flash home to the nub, with no transition bookkeeping in either direction — both fall out of *activity outranks presence* and of the return state being derived rather than remembered. Both are unit tests over a command list rather than something only reachable by holding a hotkey while saving settings.

[#47](https://github.com/Reuzehagel/draft-v2/issues/47) made `Session` a driver in the other direction too: the pill's own Dictate button starts one, and its cancel and confirm end one. The peer relationship is what made that a small change. `Session` gained three methods and an `Origin` on its phase, and learned nothing about the pill — it still only reports **session activity**, and `confirm` is literally the same call a hotkey release makes. The pill gained a second mode that shows buttons and learned nothing about dictation. Neither of the two rules the ticket cared most about needed a new mechanism: *cancel emits no flash* because a flash is what an **Outcome** produces and a cancel produces none, and *cancel is Recording-only* because `action_at` reads the derived **pill mode**, so the button stops existing at the same instant the handoff starts it.

The one place the ticket did want something new, it went to the adapter and stayed there. A click-started session is clickable without being the **button bar**, so the two questions the adapter had been answering with one predicate came apart: `WS_EX_TRANSPARENT` follows *anything pressable*, while the hover indicator, the **label** and the poll's stay-open reach follow *the bar*. Splitting them is also what makes the cancel forward-only — with the reach sized off the bar, the cursor on a button 39px out has not been holding one open, so the pill returns to the nub because it genuinely is not hovered rather than because anything reset a flag.

One consequence for the adapter is worth recording, because it looks like a lifecycle rule and is not. `Hide` and `Destroy` arrive in the same command list as the mode change that *is* the conceal, so the adapter defers them until that transition has drawn its last frame. It does not decide *whether* to hide — only that a hide it was told to perform happens after the pixels it was told to draw. The core remains the sole authority on what the pill's states are and when it leaves them.
