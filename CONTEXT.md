# Draft — Domain Glossary

The ubiquitous language for Draft's dictation flow. Use these terms exactly in code, tests, issues, and design docs — don't drift to synonyms.

## Dictation lifecycle

- **Session** — one dictation from hotkey press to pasted (or discarded) result. Owns the activation FSM(s), the live capture handle, the post-capture **Tail**, and the monotonic **session id** used to reconcile a worker's **Outcome**. The deepened `Session` module is a pure core: its methods take an event plus `now: Instant` and return **Command**s; it performs no I/O itself.
- **Session kind** — what a session is for: `Dictate` (transcript pasted at cursor) or `Command` (spoken instruction whose LLM answer is pasted). Set at start, read at stop to route the worker.
- **Tail** — the post-capture pill state: `Processing` while the worker runs, then `Done{ok}` for the terminal green/red flash. `None` when idle or recording.
- **Outcome** — a worker's terminal result for a session, reported back over a channel and matched to the session by **session id**; a stale worker's outcome (id mismatch) is ignored.
- **Command** — an effect the pure `Session` core returns for the event-loop adapter to perform: e.g. start capture, set pill mode, spawn transcription, dismiss pill. The command list is the test surface.

## Pill

- **Pill** — the overlay showing mic bars during capture, a breathing border while the worker runs, then a green/red flash. Self-animating: the `Session` core sets its logical **mode**; the pill adapter derives every frame's bars, breathing pulse, and fade itself.
- **Pill mode** — the logical state `Session` assigns the pill: `Recording` → `Processing` → `Done{ok}` → dismissed. Transitions are commands; per-frame animation is not.

## Transcription

- **Transcriber** — the seam (`trait Transcriber`) behind which every speech-to-text provider sits. `transcribe(samples) -> String`, plus `transcribe_attributed` so the provider that actually produced the text travels with the result (attribution can't race between concurrent dictations).
- **Provider** — a concrete adapter satisfying `Transcriber`: local Parakeet, Mistral, Reson8, or OpenAI/Groq via `openai_compat`. Selected by config; constructed by `transcribe::build(cfg)`, which owns key-loading, vocabulary baking, and fallback-wrapping in one place.
- **Fallback** — `FallbackTranscriber` wraps a cloud provider with the local model: on any primary error it transcribes locally instead, so a connectivity blip degrades quality rather than losing words.
- **Vocabulary hint** — a free-text decoder prompt built from the user's term list, baked into prompt-capable providers (OpenAI, Groq) at construction. Providers without biasing support ignore it; it is not part of the `Transcriber` interface.
