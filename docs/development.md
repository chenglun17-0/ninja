# Development

Contributor setup and daily commands. Design rationale lives in [PLAN.md](../PLAN.md). Packaging: [DISTRIBUTION.md](../DISTRIBUTION.md).

## Prerequisites

- Rust matching `rust-version` in the workspace `Cargo.toml` (currently 1.90).
- Zig **0.15.2** on `PATH`. Vendored Ghostty's `build.zig.zon` pins that version. Homebrew zig is the wrong one (0.16+). See [tools/README.md](../tools/README.md).
- macOS, Apple Silicon. First `cargo build` fetches the pinned Ghostty tarball via `vendor/ghostty/fetch.sh` unless `GHOSTTY_EMBED_TARBALL` points at a local copy.

## Build and test

```sh
cargo test -p ninja-protocol
cargo test -p ninja
cargo build --release -p ninja
```

Do not build or ship `ninja-preview` / `ninja-theme` from this tree; those binaries belong to ninja-plugins.

Package a signed `.app`:

```sh
scripts/package_app.sh
open dist/Ninja.app
```

Quit fully after replacing the app (not just close the window). DMG / cask: `scripts/package_dmg.sh` and [DISTRIBUTION.md](../DISTRIBUTION.md).

## Layout

```text
crates/ninja            host binary
crates/ghostty-sys      FFI
crates/ninja-protocol   wire crate + golden JSON
vendor/ghostty          pinned commit; src/ and out/ are gitignored
scripts/e2e             virtual display helper
scripts/package_app.sh  .app
scripts/package_dmg.sh  drag-install DMG + cask regen
```

Terminal config is `~/.config/ghostty/config`. Plugin enablement is `~/.config/ninja/ninja.toml`.

## E2E

GUI checks use a virtual display so they do not steal the developer's screen:

1. `scripts/e2e/virtual-display.m` — `hold [w h hidpi]` prints a `displayID`.
2. Run the host with `NINJA_E2E_SCREEN=<displayID>`.
3. Kill `hold` to unplug the display.

Screenshots use window IDs. Keyboard evidence prefers the embed API, not `CGEvent`. If no virtual display is available, fall back to the main screen and say so.

`scripts/e2e/reap.sh` kills leftover hold processes and host instances.

## Quality gates

The five standing gates from [PLAN.md](../PLAN.md) run as one command:

```sh
scripts/e2e/quality-gates.sh          # all gates, virtual display for GUI ones
scripts/e2e/quality-gates.sh --no-gui # G0–G2 only (no GUI session needed)
```

G0 unit tests, G1 protocol hygiene (dependency direction + goldens), G2 kernel-noun scan are pure logic. G3–G5 (idle invariants, enable/toggle lifecycle, crash isolation) launch the real host on the virtual display with an isolated config and a throwaway plugin, driving the panel through the same hook path as the UI (`NINJA_PANEL_PLUGIN_FILE`). Run it before pushing host or protocol changes.

## Hygiene

- Plugin nouns stay out of the kernel.
- Ghostty semantics stay in the host adapter.
- `ninja-protocol` must not depend on `ninja` or `ghostty-sys`.
- Empty-load path must not create a socket or plugin process.
