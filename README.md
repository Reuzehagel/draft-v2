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

## Building

```
cargo build --release
```

Windows only. The binary is `target/release/draft.exe`; run it with `--settings` to open the settings window directly. First run opens settings automatically. An MSI can be built from `wix/` (see the README there).

## File locations

| What | Where |
|---|---|
| Config | `%APPDATA%\Draft\config.toml` |
| Logs, history, models | `%LOCALAPPDATA%\Draft\` |
| API keys | Windows Credential Manager |
| Last recording | `%TEMP%\draft-last.wav` |

## License

MIT
