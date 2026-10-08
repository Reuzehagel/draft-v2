# Cutting a release

A release is a version bump on `main`, a tag, and a GitHub release carrying the two exes. Every release from v2.0.0 to v2.2.0 was made this way.

1. On an up-to-date `main`, bump `[package] version` in `Cargo.toml` and commit it alone as `release: X.Y.Z` (see `7459e03`). It is the one commit that goes straight to `main`, with no branch or PR. Patch for a fix, minor for anything a user can newly do.
2. `cargo build --release`, then tag the release commit with an annotated tag (`git tag -a vX.Y.Z -m vX.Y.Z`) and push the commit and the tag.
3. `gh release create vX.Y.Z target/release/draft.exe target/release/draft-cli.exe --title vX.Y.Z --notes-file <notes>`.

## Notes

Written for users, not from the commit log: what changed for them, grouped under the area they would look in (`## Providers`, `## Settings`, …), with tests, docs and refactors left out. List every PR merged since the previous tag (`git log vPREV..HEAD --oneline`) and drop the ones a user can't notice. End with the `## Downloads` section, copied from the previous release (`gh release view vPREV`): it says which exe to download.

## The MSI is not shipped

Releases attach only the exes, by choice: the update check opens the release page and users download `draft.exe`, so an installer would add a WiX toolchain and a second version to bump for no one who asked. `wix/` still builds one (`wix/README.md`), with `main.wxs` left at `Version="0.1.0"`.
