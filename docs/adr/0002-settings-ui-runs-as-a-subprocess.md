# The settings UI runs as a subprocess

The main process owns a winit event loop for the tray, the global hotkeys and the pill. `eframe::run_native` builds its own `EventLoop`, and winit permits only one per process — so the settings window cannot simply open inside the running app. Draft relaunches its own exe with `--settings`; the child owns its event loop, and the main process polls for its exit and reloads `config.toml` afterwards.

## Considered options

- **An egui window in the main process.** Requires driving egui from the existing winit loop by hand, and puts a modal, user-paced UI on the same thread as the hotkey and pill loop — the deadlock this decision exists to avoid.
- **A subprocess with an IPC channel** (pipe, socket, window message). Gives live two-way updates, and adds a protocol, a versioning problem between two builds of the same exe, and a second failure mode when the child dies mid-conversation.
- **A subprocess with config as the only channel.** Chosen.

## Consequences

- **`config.toml` is the entire interface.** The settings window never talks to the main process; it writes config, stashes API keys in the Credential Manager, and exits. Anything the two processes must agree on has to be expressible in the config file.
- **Nothing applies until the window closes.** The main process reloads on child exit, not on save, so there is no live preview of a setting.
- **Reloading is not free.** Hotkey re-registration must release the old bindings before registering the new ones — re-registering an unchanged chord collides with itself.
- **The exe has more than one entry point.** `main` dispatches on `--settings` before the single-instance gate, which is why the settings window can open while the tray app holds the mutex. The `transcribe` subcommand does *not* extend this pattern — it ships as a separate binary for unrelated reasons (see ADR 0001).
