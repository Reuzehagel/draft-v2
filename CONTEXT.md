# Draft — Domain Glossary

The ubiquitous language for Draft. Dictation is the main flow but not the only one — a **Transcription run** reaches a **Transcriber** without any of it. Use these terms exactly in code, tests, issues, and design docs — don't drift to synonyms.

## Dictation lifecycle

- **Session** — one dictation from hotkey press to pasted (or discarded) result. Owns the activation FSM(s), the live capture handle, and the monotonic **session id** used to reconcile a worker's **Outcome**. The deepened `Session` module is a pure core: its methods take an event plus `now: Instant` and return **Command**s; it performs no I/O itself. A session ends the moment its outcome is known — the terminal flash outlives it and belongs to the **Pill core**.
- **Session kind** — what a session is for: `Dictate` (transcript pasted at cursor) or `Command` (spoken instruction whose LLM answer is pasted). Set at start, read at stop to route the worker.
- **Outcome** — the terminal result of a transcription: `Delivered`, `Empty`, or `Failed`. A **Session** receives it from a worker over a channel and matches it by **session id** (a stale worker's outcome — id mismatch — is ignored); a **Transcription run** returns it as an exit code.
- **Command** — an effect the pure `Session` core returns for the event-loop adapter to perform: e.g. start capture, set pill mode, spawn transcription, dismiss pill. The command list is the test surface.

## Pill

- **Pill** — the overlay. It shows mic bars during capture, a breathing border while the worker runs, then a green/red flash — and, when resident, it stays on screen with nothing happening. Self-animating: it is given a logical **pill mode**; the pill adapter derives every frame's bars, breathing pulse, and fade itself.
- **Pill core** — the pure core that owns the pill's whole life, peer to the **Session** core rather than downstream of it. Holds **Presence** and **Activity**, derives the **pill mode** from them, and returns commands (create, destroy, show, hide, set mode) for the adapter. Dictation is only one of its drivers; the residency toggle, the fullscreen watcher, and the hover poller are the others. It owns the terminal flash's linger, so `Session` never schedules a pill change.
- **Presence** — what the pill does when no session is running: `Off` (residency toggled off), `Suppressed` (a fullscreen app has focus), or `Resident{expanded}` (on screen; `expanded` set by hover).
- **Activity** — what a session is currently asking the pill to show: `None`, `Recording`, `Processing{since}`, or `Done{ok, since}`. **Activity outranks presence**, so a session is always visible — even behind a fullscreen app — and hover can never expand a pill that is recording.
- **Pill mode** — the logical state the **Pill core** derives from **presence** and **activity** and hands the adapter: `Hidden`, `Idle`, `Expanded`, `Recording`, `Processing`, `Done{ok}`. Transitions are commands; per-frame animation is not. `Session` does not assign it — `Session` reports activity, and the pill core decides.

## Transcription

- **Transcriber** — the seam (`trait Transcriber`) behind which every speech-to-text provider sits. `transcribe(samples) -> String`, plus `transcribe_attributed` so the provider that actually produced the text travels with the result (attribution can't race between concurrent dictations).
- **Provider** — a concrete adapter satisfying `Transcriber`: local Parakeet, Mistral, Reson8, or OpenAI/Groq via `openai_compat`. Selected by config; constructed by `transcribe::build(cfg)`, which owns key-loading, vocabulary baking, and fallback-wrapping in one place.
- **Fallback** — `FallbackTranscriber` wraps a cloud provider with the local model: on any primary error it transcribes locally instead, so a connectivity blip degrades quality rather than losing words.
- **Vocabulary hint** — a free-text decoder prompt built from the user's term list, baked into prompt-capable providers (OpenAI, Groq) at construction. Providers without biasing support ignore it; it is not part of the `Transcriber` interface.
- **Transcription run** — one execution of the `transcribe` subcommand: a media file in, text out. Not a **Session** — it has no activation, no capture, no pill, and no paste. It shares only the **Transcriber** and the **Replacements**.

## Post-processing

- **Replacements** — the user's find/replace rules, applied to any transcript whatever produced it. They correct vocabulary, so they are true of a **Session** and a **Transcription run** alike.
- **Voice commands** — spoken instructions to Draft ("new paragraph") turned into their effect. They belong to a **Session** only: a recorded file's speaker is not addressing Draft, so applying them there corrupts text that merely contains the phrase.
