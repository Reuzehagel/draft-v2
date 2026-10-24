# Fish Audio speech-to-text as a Provider, and Phonon-2 as a local model

Researched 2026-09-30. Primary sources only: Fish Audio's docs, OpenAPI spec, official Python SDK, pricing page, terms and privacy policy; Fermion Research's site, Hugging Face model card, GitHub repo and PyPI package. Nothing here was tested against a live API: no Fish key was used and no model was run. Anything a source did not say is listed under [Unconfirmed](#unconfirmed).

## Recommendation

- **Fish Audio: do not add it yet.** It clears the mechanical half of the **provider bar** (one key, one inline request), but nothing first-party shows it beating an existing **Provider** on accuracy, latency, price or free tier. It has no field for the **vocabulary hint**, Dutch is not among the languages Fish says are well tested, the endpoint is labelled BETA, and the terms allow training on uploads. A 30-minute spike with a real key (Dutch and English clips, timed, both models) would settle the open questions cheaply. If it wins that spike, add it as a plain Provider on `transcribe-1`.
- **Two models per Session kind (`transcribe-1` for Dictate, `transcribe-1-pro` for Command): do not build it.** It contradicts two rules in `GLOSSARY.md`, the benefit is speculative, and the Pro output format is undocumented for English.
- **Phonon-2: not viable today.** English only, no Windows library, no C API, no ONNX. The only Windows route is Python plus PyTorch. Draft's current Parakeet v3 is the multilingual teacher Phonon-2 was distilled from, and is slightly more accurate on Fermion's own table.

## Fish Audio

### 1. Request contract

| Item | Value | Source |
| - | - | - |
| Endpoint | `POST https://api.fish.audio/v1/asr` | [API reference](https://docs.fish.audio/api-reference/endpoint/openapi-v1/speech-to-text) |
| Auth | `Authorization: Bearer <key>` (`BearerAuth`, http bearer) | same, OpenAPI block |
| Model | `model` **HTTP header**, enum `transcribe-1` \| `transcribe-1-pro`, default `transcribe-1`, not required. "`model` is not a form field." | [OpenAPI spec](https://docs.fish.audio/api-reference/openapi.json), [guide](https://docs.fish.audio/features/speech-to-text) |
| Body | `multipart/form-data` or `application/msgpack`. JSON with base64 audio is not supported. | API reference |
| `audio` | required, binary file part | OpenAPI spec |
| `language` | optional string, default null. "Optional hint. The language is auto-detected regardless." | OpenAPI spec |
| `ignore_timestamps` | optional bool, default `true` | OpenAPI spec |
| 200 response | `{text, duration, segments[], language_code?, language?}`; `text`, `duration`, `segments` required | OpenAPI spec |
| Errors documented | 401, 402 (out of credits), 503 (high load); quickstart also lists 429 | OpenAPI spec, [quickstart](https://docs.fish.audio/developer-guide/getting-started/quickstart) |
| Status | "This BETA endpoint" | API reference |

- Those three body fields and the one header are the whole contract. The official SDK sends exactly `audio`, `ignore_timestamps`, and `language` as msgpack and has no `model` argument: [fish-audio-python `resources/asr.py`](https://github.com/fishaudio/fish-audio-python/blob/main/src/fishaudio/resources/asr.py).
- WAV is accepted: "Common formats work directly: `wav`, `mp3`, `opus`, and more. Send the raw file bytes" ([guide](https://docs.fish.audio/features/speech-to-text)). No sample-rate, channel or bit-depth requirement is stated anywhere, so Draft's 16 kHz mono 16-bit WAV (`src/transcribe/mod.rs:266-285`) is inside what is documented but not explicitly named.
- The response has a top-level `text`, so `parse_text_response` (`src/transcribe/mod.rs:252-264`) works unchanged.
- The SDK's `ASRResponse` docstring says `duration` is in milliseconds ([`types/asr.py`](https://github.com/fishaudio/fish-audio-python/blob/main/src/fishaudio/types/asr.py)); the docs and spec say seconds. Draft would not read the field.

### 2. Vocabulary hint

None. The OpenAPI schema has no prompt, keyword, phrase or biasing property, and the SDK sends none. Fish would be a Provider that ignores the **vocabulary hint**, which `GLOSSARY.md:60` permits ("one that cannot ignores it") but which is a regression against every other cloud Provider since #107.

### 3. Pricing

- `transcribe-1`: $0.36 per audio hour. `transcribe-1-pro`: $0.36 per audio hour. Billed on audio duration, rounded up to the nearest second. Pay-as-you-go, no subscription or minimum. Source: [pricing and rate limits](https://docs.fish.audio/developer-guide/models-pricing/pricing-and-rate-limits).
- That is $0.006 per minute, the same for both models.
- Free tier: the API docs name a free TTS model (`s2.1-pro-free`) and no free ASR model. The marketing page says "8,000 credits" free every month, in a sentence about the web app ([fish.audio/stt](https://fish.audio/stt/)). Whether any of that applies to API calls is unconfirmed.

### 4. Latency and streaming

- No first-party latency figure for either ASR model was found.
- `ignore_timestamps=false` is slower: "this will increase the latency in audio shorter than 30 seconds" (OpenAPI spec) and "Alignment adds processing time" (guide). The default `true` is the fast path, so Draft would send nothing or `true`.
- No streaming ASR endpoint exists in the API. The OpenAPI spec's speech paths are `/v1/asr`, `/v1/tts`, `/v1/tts/stream/with-timestamp`, `/v1/tts/live/with-timestamp`; the AsyncAPI spec and the [real-time streaming page](https://docs.fish.audio/developer-guide/best-practices/real-time-streaming) cover TTS only. The marketing page's "Real-time Transcription" describes the web app.

### 5. Limits

- "Through the API, each request accepts files up to 20 MB and 60 minutes" ([fish.audio/stt](https://fish.audio/stt/) FAQ). This is first-party but appears only on the marketing page, not in the API reference.
- At 16 kHz mono 16-bit (32,000 bytes/s) 20 MB is about 10.9 minutes. Draft's ten-minute **duration cap** produces about 19.2 MB, so a **Transcription run** fits, narrowly.
- Rate limit is concurrency, not QPS: 5 concurrent requests under $100 paid, 15 from $100, 50 from $1,000 ([pricing](https://docs.fish.audio/developer-guide/models-pricing/pricing-and-rate-limits)). Irrelevant for a single-user dictation tool.
- One request is one audio file; long recordings must be split client-side (guide).

### 6. Languages

- "80+ languages with automatic detection", "with English, Mandarin, Cantonese, Japanese and Korean the most thoroughly tested through the API" ([fish.audio/stt](https://fish.audio/stt/)). No ASR language list is published in the docs. **Dutch is not named anywhere for ASR**; the only Dutch mention is the S1 TTS list ([models overview](https://docs.fish.audio/developer-guide/models-pricing/models-overview)).
- Omitting `language` auto-detects, and detection runs even when a hint is given: "it does not force the returned language" (guide). This fits Draft's rule of never sending a language (`GLOSSARY.md:57`).
- Fish's own cookbook says a hint "is more reliable than auto-detection when you already know the source language, especially for phonetically similar languages" ([cookbook](https://docs.fish.audio/developer-guide/sdk-guide/cookbook/batch-transcribe-with-language-hint)). That is a warning sign for a bilingual Dutch/English user on short clips.

### 7. `transcribe-1-pro` output

- Speaker markers: inline `<|speaker:N|>` in `text`, N numeric, applying until the next marker. No separate speaker list, no per-segment speaker id (guide, API reference).
- Emotion and vocal-event cues: inline bracketed text. The only two examples in all of Fish's docs are `[高兴]` and `[laughter]`. "The API has no separate `emotion` field or fixed emotion enum" (guide). There is no inventory to enumerate: the set is open.
- The one emotion example is Chinese inside a Chinese transcript; the one event example is English. No English-speech example is published, so the language of cues for English or Dutch speech is unconfirmed.
- No request field turns markers or cues off. The contract has three fields and none concerns them. The only way to get a clean Pro transcript is to strip client-side. The docs note that `segments` text has markers removed, but segments require alignment, which is the slow path.
- Whether single-speaker audio still starts with `<|speaker:0|>` is not stated. The API reference says "Pro includes inline speaker markers" without qualification.
- Whether `transcribe-1` ever emits either kind of tag is not stated. The docs attribute both only to Pro.

### 8. Data handling

- Training: "Usage Data and Content may be used to develop, train, or enhance artificial intelligence or machine learning models that are part of Fish.Audio's products and services ... and you authorize Fish.Audio to process your Usage Data and Content for such purposes" ([terms](https://fish.audio/terms/)). "Content" includes "file uploads" ([privacy policy](https://fish.audio/privacy/)). No API-specific carve-out or opt-out was found.
- Retention: "We retain Content as long as we need it to ensure that our systems are working appropriately, effectively and efficiently" (privacy policy). No fixed period.
- Region: no region choice in the API. European personal data "will be further transferred to, and stored at, a destination outside of the European Region" (privacy policy).
- The marketing page says "Your audio is processed only for transcription, never shared" ([fish.audio/stt](https://fish.audio/stt/)), which the terms do not back.

### Provider bar (`GLOSSARY.md:58`)

| Test | Result |
| - | - |
| Signup yields a single API key | Pass (key from the dashboard; prepaid balance) |
| One request carrying the audio inline | Pass |
| Dedicated speech-to-text endpoint, not a chat model | Pass |
| Beats an existing Provider on something a user would feel | **Not shown.** No accuracy or latency numbers exist; price was not compared here against the other Providers' current rates; an API free tier is unconfirmed. |

## Phonon-2

### 9. What ships, and whether it links on Windows

- The Hugging Face repo holds one model archive, `phonon-2.bps.tar.zst` (163,515,201 bytes, unpacking to a `model.fermion` container), two Python reference scripts, licences and a notice: [file list](https://huggingface.co/FermionResearch/Phonon-2/tree/main), [`config.json`](https://huggingface.co/FermionResearch/Phonon-2/blob/main/config.json). `library_name: mlx`. No ONNX file.
- The GitHub repo ([fermionresearch/phonon](https://github.com/fermionresearch/phonon)) is Python: a CLI, MLX and CUDA engines, Dockerfiles. Its only native artefacts are prebuilt Linux shared objects, `docker-cpu/bin/libphonon_cpu-linux-x86_64.so` and `docker/bin/libphonon_fold4_cuda.so`. The CPU image README says the kernel is "binary only. No kernel source ships in this image" ([docker-cpu/README.md](https://github.com/fermionresearch/phonon/blob/main/docker-cpu/README.md)).
- No C header, no documented C API, no Windows DLL or import library, no Rust binding, no ONNX export.
- The site says the CPU encoder "runs in C" ([research page](https://www.fermionresearch.com/research/phonon-2/)). That describes the Linux kernel above, not a library anyone can link.
- Windows support is Python: `pip install fermion-research torch safetensors soundfile scipy zstandard`, x86-64 only, and may need `vc_redist.x64.exe` ([docs/cpu.md](https://github.com/fermionresearch/phonon/blob/main/docs/cpu.md), [docs/install.md](https://github.com/fermionresearch/phonon/blob/main/docs/install.md)). The PyPI package depends on `torch>=2.3` and `transformers>=5.0` ([PyPI](https://pypi.org/project/fermion-research/)).
- The only integration that needs no Python inside Draft is the CLI's own server: `fermion serve phonon-2` exposes an OpenAI-compatible `POST /v1/audio/transcriptions` on `127.0.0.1:8000` (repo README). Draft's `openai_compat` adapter could talk to it, but the user would have to install and run Python and PyTorch themselves, which defeats the point of a 164 MB model.
- Licences: weights CC-BY-4.0 (derived from NVIDIA `parakeet-tdt-0.6b-v3`); the CLI and repository code Apache-2.0 ([HF NOTICE](https://huggingface.co/FermionResearch/Phonon-2/blob/main/NOTICE), repo README). The Linux kernel binary has no source, and its licence is not stated separately.

### 10. Benchmarks against Parakeet TDT v3, and languages

From Fermion's model card (Open ASR Leaderboard sets, WER %, lower is better):

| Model | Download | LS clean | LS other | AMI | Earnings-22 | GigaSpeech | SPGISpeech | VoxPopuli | Average |
| - | -: | -: | -: | -: | -: | -: | -: | -: | -: |
| Phonon-2 | 164 MB | 1.72 | 3.92 | 9.37 | 6.96 | 8.35 | 3.70 | 2.46 | 5.21 |
| Parakeet TDT 0.6B v3 (teacher) | 2,508 MB | 1.52 | 3.13 | 9.42 | 5.85 | 7.99 | 3.63 | 3.19 | 4.96 |

Source: [Hugging Face model card](https://huggingface.co/FermionResearch/Phonon-2).

- Parakeet v3 is more accurate on average (4.96 against 5.21) and on five of seven sets. Phonon-2 wins on AMI and VoxPopuli.
- The 2,508 MB row is the full-precision teacher. Draft runs the int8 ONNX export of the same model (`src/transcribe/parakeet_download.rs:11-21`, `src/transcribe/parakeet.rs:39`), about 700 MB. Fermion publishes no accuracy row for the int8 build, so the real gap to what Draft ships is unknown.
- Speed: "Windows x64, 8 vCPU: 21.0x" realtime, the slowest surface in Fermion's table (Linux x86-64 on eight Zen 5 cores is 142.8x). On an M5 MacBook Air Fermion measured sherpa-onnx Parakeet v3 int8 at 16.5x and Phonon-2 on MLX at 174x ([research page](https://www.fermionresearch.com/research/phonon-2/)). No Windows comparison against Parakeet is published.
- Languages: English only. "We are releasing Phonon-2 today as an open model for English" (research page); "Input envelope: English, 16 kHz audio" (docs/cpu.md). The teacher is multilingual; the student is not.
- Roadmap: the only forward-looking statement is "A Core ML runtime for Apple devices is coming soon" (research page). Nothing about other languages or a Windows library.

Moving Draft's local model to Phonon-2 would save roughly 540 MB of download and cost Dutch entirely, which is a regression against the "no language is ever sent, each clip is auto-detected" rule in `GLOSSARY.md:57`.

## Codebase fit

### 11. Where a new `Provider` variant is threaded

Required, or the build or a test fails:

| Place | What |
| - | - |
| `src/config.rs:185-192` | the `Provider` enum variant (serde `snake_case` gives the config name) |
| `src/config.rs:197-206` | `Provider::label` match arm |
| `src/secrets.rs:11-20` | `slot_name` match arm (keyring slot) |
| `src/secrets.rs:22-31` | `env_var` match arm |
| `src/transcribe/mod.rs:5-11` | `pub mod` for the new adapter |
| `src/transcribe/mod.rs:160-194` | `build_primary` match arm: load key, construct |
| `src/transcribe/<new>.rs` | the adapter, modelled on `src/transcribe/elevenlabs.rs` (multipart, `parse_text_response`) |
| `src/settings_ui/state.rs:13-20` | `ALL_PROVIDERS`; the list is not compiler-checked, so omission silently hides the Provider from the dropdown (`src/settings_ui/mod.rs:729-732`) and from the key store (`state.rs:23-42`) |

Needs no change: the key editor and dropdown iterate `ALL_PROVIDERS` and `label()` (`src/settings_ui/mod.rs:729-758`, `1156-1199`); the tray tooltip uses `label()` (`src/tray.rs:56`); history takes the adapter's `name()` string (`src/main.rs:505`); `transcription_run.rs:87` goes through `transcribe::build`; an old build reading the new name falls back to local Parakeet (`src/config.rs:216-232`).

Documentation that names the Providers: `GLOSSARY.md:57`, `README.md:9` (which already omits ElevenLabs).

An adapter for Fish would differ from `elevenlabs.rs` in three ways: the part is named `audio`, auth is `bearer_auth`, and `new` would accept `terms` and drop them (or not take them), since there is no field to carry them.

### 12. Different models for Dictate and Command

How it works today:

- There is one **Transcriber** for the whole process. `transcribe::build(&cfg)` is called at launch (`src/main.rs:110`) and on config reload (`src/main.rs:1247`) and stored in `App::transcriber` (`src/main.rs:271`).
- `spawn_worker` clones that one handle for both **Session kinds** (`src/main.rs:399`) and calls it before it branches on kind (`src/main.rs:451`). The branch comes after: `Command` sends the trimmed transcript to `llm::run_command`, `Dictate` runs the postprocess pipeline (`src/main.rs:472-492`).
- So push-to-command shares the dictation Transcriber exactly, including the **Fallback** wrapper and the local model.

What would have to change:

1. `transcribe::build` takes only `&Config` (`src/transcribe/mod.rs:101`). It needs a purpose parameter, or a second entry point, so the Fish adapter can be built with either model header.
2. `App` holds a second `Option<Arc<dyn Transcriber>>`, built at both call sites (`main.rs:110`, `main.rs:1247`), and `spawn_worker` picks by `kind` at `main.rs:399`.
3. `session.set_transcriber_available` (`main.rs:116`, `main.rs:1249`) is one flag; it stays correct only if both are built or neither.
4. The model reaper is handed one Transcriber (`main.rs:118`, `main.rs:1253`). Two `FallbackTranscriber`s would each wrap their own `ParakeetTranscriber`, so either the local model is shared by `Arc` between them or it can be resident twice.
5. A marker stripper for `<|speaker:N|>`, and for the Dictate path with Pro a cue stripper too. The cue set is open and its language unknown, so the stripper is "remove every bracketed span", which also deletes brackets the user actually dictated into, or that the model transcribed from speech.
6. `llm::SYSTEM_PROMPT` (`src/llm.rs:21-25`) tells the model its whole reply is pasted verbatim. It would need to be told what the cues are, or it may echo `[laughter]` into the paste.
7. A setting for "use Pro for dictation", a row in the Transcription pane, and its config key.

Reasons not to do it:

- It breaks "**One Provider, one model, chosen by Draft**: there is no model setting" (`GLOSSARY.md:57`). The opt-in toggle is a model setting by another name.
- It breaks "any switch a vendor offers that tidies the words — filler removal, a 'smart' mode, tagged audio events — is pinned to verbatim" (`GLOSSARY.md:57`). `elevenlabs.rs:58-60` pins `tag_audio_events=false` for exactly this reason. Choosing Pro is choosing tagged events.
- It only exists when the selected Provider is Fish. Every other Provider, and the local **Fallback**, give the Command path no cues. The LLM prompt would have to work both with and without them, so the cues can only ever be a nice-to-have.
- The value is speculative. A push-to-command utterance is a short instruction ("rewrite this more formally"). No evidence shows that a `[happy]` tag improves a Groq answer, and a wrong cue can steer it.
- Pro is sold for "multi-speaker conversations". Dictation and commands are one speaker. Speaker markers are pure noise here, and whether they appear on single-speaker audio is undocumented.
- Both models cost the same, so there is no price reason to split them either way.
- The format is a BETA, undocumented for English, with no off switch. Draft would be parsing a string convention that Fish can change without a schema change.
- It doubles the state the reload path has to keep consistent (items 2 to 4) for one Provider.

If emotion-aware commands are ever wanted, the cheaper shape is a separate decision about the Command path's input, made after Fish has first earned a place as a plain Provider.

## Unconfirmed

Not found in any primary source; do not rely on these.

- Fish: any latency figure, for either model.
- Fish: any accuracy figure (WER) for `transcribe-1` or `transcribe-1-pro`.
- Fish: whether Dutch is supported at all, and how well. Only "80+ languages" is claimed, with five others named as best tested.
- Fish: whether the free monthly credits apply to the API, or whether a new API account has any free balance.
- Fish: the 20 MB / 60 minute limit appears only on the marketing FAQ, not in the API reference or spec.
- Fish: accepted sample rates, channel counts and bit depths. WAV is listed; 16 kHz mono 16-bit is not specifically named.
- Fish: whether Pro emits `<|speaker:0|>` for single-speaker audio.
- Fish: what cues look like for English or Dutch speech, and in which language they are written.
- Fish: whether `transcribe-1` ever emits speaker markers or cues.
- Fish: how each model handles dictated punctuation, numbers and code-switching inside one clip.
- Fish: processing region, and any zero-retention or no-training option for API customers.
- Fish: the launch announcement at https://x.com/FishAudio/status/2104617131920748857 could not be fetched (HTTP 403). Secondary coverage dates it 28 September 2026; nothing in this note depends on it.
- Fish: how the other Providers' current prices compare with $0.36 per hour; not researched here.
- Phonon-2: accuracy against the int8 ONNX Parakeet v3 that Draft actually ships.
- Phonon-2: Windows speed against Parakeet v3 on the same machine.
- Phonon-2: any plan for a C API, a Windows library, an ONNX export or more languages.
- Phonon-2: the licence of the prebuilt Linux kernel binaries.
