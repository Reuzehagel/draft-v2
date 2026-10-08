# Building the Draft MSI

Per-user MSI (no admin required). Installs to
`%LOCALAPPDATA%\Programs\Draft` and adds a Start Menu shortcut.

## One-time setup

1. Install WiX Toolset v3.14:
   <https://github.com/wixtoolset/wix3/releases/latest>
   (download the `.exe` and run it).
2. Install `cargo-wix`:
   ```powershell
   cargo install cargo-wix
   ```

## Build

```powershell
cargo build --release
cargo wix --no-build --nocapture
```

The `.msi` lands in `target\wix\`.

## What gets installed

- `draft.exe`
- `DirectML.dll` (placed in `target\release` by the `ort` build script;
  onnxruntime itself is statically linked into `draft.exe`. If the DLL
  isn't there after `cargo build --release`, run a clean build — `ort`
  only copies it when the link step actually runs)
- Start Menu shortcut → `[APPLICATIONFOLDER]draft.exe`

## What is _not_ installed

- The Parakeet model files (~670 MB). The user downloads those on first
  run via the Settings window.
- The HKCU `Run` autostart entry. Draft manages that itself so the user's
  choice survives reinstalls and upgrades.
- API keys. They live in Windows Credential Manager and aren't touched by
  the installer or uninstaller.

## Not part of releases

Releases ship `draft.exe` and `draft-cli.exe` only (see
`docs/agents/release.md`), so a release bumps `Cargo.toml` alone and
`<Product Version="...">` in `main.wxs` is left at `0.1.0`. To build an MSI
anyway, set that version to the release's first.

The `UpgradeCode` GUID in `main.wxs` is stable across versions — that's
what tells Windows "this MSI replaces the previous Draft install."
