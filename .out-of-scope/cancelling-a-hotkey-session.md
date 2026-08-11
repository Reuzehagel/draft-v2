# Cancelling a hotkey-started dictation

Draft does not offer a way to abort a dictation session started by the hotkey. Once the chord is held and audio is capturing, releasing commits: the samples go to a worker, a transcript comes back, it is written to history and pasted at the cursor.

## Why this is out of scope

**The workaround is already better than the feature.** Stop talking, release the chord, and remove the text it types. The transcript arrives at the cursor in the application the user is already working in, which is an application that has undo. Deleting a sentence that just appeared is a gesture every user already owns, costs one keystroke, and needs no discoverability. A cancel gesture would have to be learned, and it would have to be learned for a situation — "I started dictating by accident" — that ends in a small amount of unwanted text either way.

**What it would cost is not small.** The dictation path has one road out of Recording, and it goes through the worker:

- `activation.rs` — `OutEvent` is `Start | Stop | Ignore`. A cancel is a third exit from the FSM, in every mode: hold, toggle, and the double-press lock.
- `session.rs` — `Command` is `StartCapture | SetPill | SpawnTranscription | DismissPill`, and `Phase::Recording → end` always spawns. A cancel is a new command, a new phase transition, and a new terminal state that isn't an `Outcome` (it never produces a transcript, so it can't be `Delivered`/`Empty`/`Failed`).
- `hotkey.rs` — `Chord` is `Dictate | Command`. Any gesture that isn't the existing chords is a third registration, a config key, a settings row, and another binding in the re-registration dance in `reload_config`.
- The pill — a hotkey session renders the bare recording pill and is click-through, so there is nowhere to show that cancelling is possible, and no acknowledgement when it happens.

That is a feature spanning four modules and the config surface, to save a keystroke of undo.

**"Escape while recording" is not the cheap version.** Draft has no keyboard hook beyond its registered chords and the pill never takes focus, so Escape is not a binding — it is either a fourth global registration (stealing Escape system-wide while recording) or a low-level keyboard hook. Both are worse than the problem.

## The asymmetry with the click path is deliberate

[#29](https://github.com/Reuzehagel/draft-v2/issues/29) gives a **click**-started session a cancel button: `[×] ~~~~~ [✓]`. That is not an inconsistency to be fixed by giving the keyboard the same thing.

A click-started session has **no other stop gesture** — without `×` and `✓` you would click the recording pill and hope. A hotkey session already has one in the hand that started it. The buttons exist in the click path because they are the only control there; adding them to the keyboard path would be symmetry for its own sake.

The rule stands as: **the pill shows controls only when it has to.**

## Prior requests

- [#34](https://github.com/Reuzehagel/draft-v2/issues/34) — "A hotkey-started dictation cannot be cancelled"
