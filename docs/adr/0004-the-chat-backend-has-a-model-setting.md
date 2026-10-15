---
status: accepted
---

# The Chat backend has a model setting; Providers still don't

A **Provider** has no model setting — "One Provider, one model, chosen by Draft" — because a user has no way to judge that choice: a transcript is either the words they said or it isn't, and Draft follows the vendor to its better model on the user's behalf. The **Chat backend** behind push-to-command breaks both halves of that reasoning, so it gets the setting Providers are refused.

## Why the Chat backend is different

- **A user can judge a chat answer.** The answer is the whole product of a push-to-command run: its tone, length and quality are things a user sees and has opinions about, and two models really do answer the same instruction differently. That is a preference, and preferences are settings.
- **Vendors retire chat models under us.** Groq retired Llama 3.3 70B, push-to-command's original model, for free and developer tiers on 2026-08-16 (#109), and the fix had to ship as a release. Chat model ids churn on a scale of months; a speech-to-text endpoint does not. With the model a setting, the next retirement is fixed in Settings > Commands, by the user, the same day — the error names the retired model and says where to choose another.

## What the setting is, and isn't

- **Draft lists the models it knows how to drive.** Each listed model carries the request fields that keep its reasoning out of the answer (`reasoning_effort`, `reasoning_format`, `include_reasoning` — they differ per model and per backend), because whatever the model returns is pasted at the cursor. A model's thinking pasted into someone's email is the worst failure this feature has.
- **"Other…" takes any id and sends it as-is**, with none of those fields: Draft can't know which ones an unlisted model accepts, and a field a model rejects fails the request. The settings window says so plainly, next to the field.
- **Draft never falls back to another model.** A retired or unknown model is an error naming it. A silent swap would change the answers a user chose a model for, and hide the one thing they need to act on.
- **The default follows the keys.** Cerebras with `qwen-3.8-27b` unless only a Groq key is stored, in which case Groq with `openai/gpt-oss-120b` — so an existing Groq user's push-to-command keeps working with nothing to configure. Until the user chooses, nothing is written to `config.toml`, and the pick is made afresh on every run, so adding a Cerebras key later moves it. Once the user picks anything, even what Draft would have picked, it is written down and no key change moves it.

## Consequences

- Cerebras is a Chat backend only. It has its own keyring slot (`cerebras_api_key`, overridable with `CEREBRAS_API_KEY`) but is not a Provider and never appears in the Provider dropdown; keyring slots are now keyed by `secrets::KeySlot` rather than by Provider. The Groq Chat backend uses the Groq Provider's slot.
- An unrecognised backend in `config.toml` loads as no choice — Draft's default — with a warning, the rule an unrecognised `provider` already follows.
- Push-to-command's toggle and hotkey moved from the Recording pane to a new Commands pane beside the backend, the keys and the model. The Cerebras key row is always there, since it has nowhere else to live; the Groq key row joins it while Groq is chosen. The config keys are unchanged, so existing configs keep their values.
- The listed models' reasoning fields have to be re-checked against the vendors' docs when a model is added or a backend changes its API: https://console.groq.com/docs/reasoning and https://inference-docs.cerebras.ai/capabilities/reasoning.

## Status

Built in [#111](https://github.com/Reuzehagel/draft-v2/issues/111).
