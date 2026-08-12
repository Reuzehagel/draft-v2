# Draft

Push-to-talk speech-to-text for Windows. Hold a hotkey, speak, release — the transcript lands at your cursor.

Draft runs in the system tray. While you hold the hotkey (`Ctrl+\` by default), a small pill overlay shows your microphone level. On release, the audio is transcribed and pasted into whatever app has focus. A toggle mode and a double-press lock exist for longer dictations.

## Transcription

The default engine is Parakeet, an on-device model (~700 MB, downloaded from Settings) that needs no API key and works offline. Cloud providers are also supported: Mistral, Groq, OpenAI, and Reson8, with API keys stored in the Windows Credential Manager. When a cloud provider errors out mid-dictation, Draft can fall back to the local model instead of losing what you said.

## What happens to the transcript

Before pasting, the transcript runs through a deterministic pipeline:

- Spoken commands: "new line", "new paragraph", "scratch that" (deletes the previous segment), "all caps" (uppercases the next word).
- Find/replace rules, in order, with optional whole-word and case-sensitive matching.
- Custom vocabulary biasing for providers that accept a prompt hint (OpenAI, Groq).

Every transcript is saved to a local history before the paste is attempted, so a paste that lands in the wrong window is recoverable. The tray menu has a "copy last transcript" entry for exactly that case.

There is also an optional second hotkey, push-to-command: speech is treated as an instruction, sent to an LLM (Groq), and the *answer* is pasted instead of your words.

## Transcribing a file

`draft-cli.exe` ships alongside `draft.exe` and transcribes a recording you already have:

```
draft-cli transcribe meeting.mp4 > meeting.txt
```

It reads mp3, mp4, m4a, wav, flac and alac, up to ten minutes, using the provider and API key you already configured — and it runs happily while the tray app is running. Only the transcript goes to stdout, so redirecting it gives you the words and nothing else; anything else to say goes to stderr. It exits `0` on success (including a file with no speech, which prints nothing), `1` if the file can't be read or the provider fails, and `2` if the file is over the ten-minute limit.

Your find/replace rules are applied. Spoken commands are *not* — someone on a recording saying "new paragraph" meant those words. Nor is anything added to your dictation history.

## Building

```
cargo build --release
```

Windows only. This produces two binaries: `target/release/draft.exe`, the tray app (run it with `--settings` to open the settings window directly; first run opens settings automatically), and `target/release/draft-cli.exe`, the console command above. An MSI installing both can be built from `wix/` (see the README there).

## File locations

| What | Where |
|---|---|
| Config | `%APPDATA%\Draft\config.toml` |
| Logs, history, models | `%LOCALAPPDATA%\Draft\` |
| API keys | Windows Credential Manager |
| Last recording | `%TEMP%\draft-last.wav` |

## License

MIT
