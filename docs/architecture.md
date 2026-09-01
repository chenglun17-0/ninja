# Architecture

Read this before changing `crates/ninja` or `crates/ninja-protocol`. Product constraints: [PRODUCT.md](../PRODUCT.md). Implementation lock: [PLAN.md](../PLAN.md). Wire types: [`crates/ninja-protocol`](../crates/ninja-protocol/src/lib.rs).

Ninja is a macOS GPU terminal **host**. libghostty owns PTY, VT, GPU drawing, fonts, themes, and keybinds. The host owns windows, tabs, splits, and a process-isolated ADE protocol. Plugins never link the host and never link `ghostty.h`.

```text
~/.config/ghostty/config  →  libghostty
~/.config/ninja/ninja.toml  →  plugin supervisor
                             hit / layer / input / spawn / config / theme / pane
                             Unix socket, u32le + JSON
```

## Empty load

`[plugins] enabled` empty (the default) means: no ADE socket, no plugin process, no pump timer. The app is still a complete terminal — multi-window, native tabs, splits, Ghostty config.

Enabled plugins start with the app. Disable (panel off) kills the child, closes its layers, revokes a theme override, and if the enabled list is empty, deletes the socket. That is the "off means light" gate.

## Crates

| Crate | Owns |
| --- | --- |
| `ninja` | AppKit shell, Ghostty adapter, plugin supervisor, plugin panel |
| `ghostty-sys` | bindgen of vendored `ghostty.h` |
| `ninja-protocol` | Versioned messages and frame codec only |

Official plugins live in **ninja-plugins** and install as `~/.config/ninja/plugins/<name>`. They are not workspace members of this repository and are not in the `.app`.

## Hits

A click is not "open this file". The host broadcasts a `hit` (path / URL / OSC-8). A plugin may `claim` or `ignore`. Nobody claims → `/usr/bin/open`, same as Ghostty.

Ghostty-specific behavior stays in the host adapter: ⌘+hover links, `file://` OSC-7, grid-token fallback when no hyperlink exists. None of that is in the protocol.

## Layers

A layer is `placement` × `surface`:

- placement: `overlay` / `side` / `tab`
- surface: `pixels` (IOSurface) / `html` (WKWebView)

The kernel has no plugin nouns (`preview`, `editor`, `save`, `git`, `lsp`). HTML layers load a document and pass opaque `layer.msg` frames. Overlay/side do not host HTML.

Esc / host close policy always closes the layer. Focus returns to the terminal.

## Plugin processes

The supervisor binds `${TMPDIR}/ninja-ade-{pid}.sock` only when at least one plugin is enabled. Children get `NINJA_ADE_SOCK`. Binary lookup:

1. `[plugins.paths]`
2. `$NINJA_PLUGIN_DIR/<name>`
3. `~/.config/ninja/plugins/<name>`
4. directory of the host executable (dev layout only)

Replacing an enabled binary (mtime change) restarts that child. Dropping a new file into the plugin directory makes it visible in the panel; it does not auto-enable.

The panel (`⌘,`, Ghostty action `toggle_visibility`) lists installed names plus enabled/error names. Toggle writes `ninja.toml` and starts or stops the process. The panel does not steal terminal focus.

## What is not here

No in-process plugin VM. No marketplace. No second keymap. No WK APIs beyond load-document + `layer.msg`. New protocol types need a second independent plugin, not a convenience for one.
