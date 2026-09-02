<p align="center">
  <img src="assets/icon-source.png" width="128" alt="Ninja" />
</p>

<h1 align="center">Ninja</h1>

<p align="center">
  <b>A terminal that stays a terminal.</b><br />
  Fast when empty. Plugins when you need them.
</p>

<p align="center">
  <img alt="platform" src="https://img.shields.io/badge/platform-macOS-111111" />
  <img alt="engine" src="https://img.shields.io/badge/engine-libghostty-4C8BF5" />
  <img alt="plugins" src="https://img.shields.io/badge/plugins-out--of--process-2ea44f" />
</p>

Ninja is an **augmentable terminal**. Open it and you get a GPU terminal — windows, tabs, splits, your Ghostty config. No sidebar. No agent panel. No IDE chrome.

When a command isn't enough, a plugin can claim the click: open a file, restore an agent session, push a theme. Disable the plugin and the extra process is gone. The app is still a complete terminal.

## Why

Terminals are stuck at two poles:

- **Heavy** — agent IDEs that are also terminals. Capable, expensive at idle.
- **Bare** — excellent GPU terminals with almost no extension surface. Click a path, get Finder.

Ninja is neither a light IDE nor “Ghostty plus a plugin marketplace.” It is a **host**: Ghostty for the terminal, a narrow protocol for everything else.

> You come here to run commands. Plugins appear only when commands are not enough.

## Features

- **Idle is Ghostty-class.** No plugins loaded means no plugin processes, no plugin sockets, no plugin memory. Tabs, splits, and keybindings still work.
- **Ghostty underneath.** PTY, VT, GPU rendering, fonts, themes, and keybinds are libghostty. `~/.config/ghostty/config` is the terminal config. Ninja does not invent a second one.
- **Click is a protocol, not a hard-coded open.** Path / URL / OSC-8 hits are broadcast. A plugin may claim them. Nobody claims → same as Ghostty (`open`).
- **Layers, not chrome.** A plugin can ask for overlay, side, or a full tab; pixels or HTML. The host does not know “editor” or “preview.”
- **Plugins are processes.** They talk JSON over a Unix socket. They never link the host, never link `ghostty.h`. Crash a plugin, keep the terminal.
- **Zero plugins in the app you ship.** Preview, editor, agents, themes — install them yourself. Uninstall them and the product is still Ninja.

## Install

macOS only (Apple Silicon). This repository is not a public release; there is no notarized cask yet.

```sh
git clone https://github.com/chenglun17-0/ninja.git
cd ninja
# zig 0.15.2 on PATH — see tools/README.md (not brew's zig)
scripts/package_app.sh
open dist/Ninja.app
```

Quit fully after installing a new build (not just close the window).

## Configure

Terminal, window, and keys:

```text
~/.config/ghostty/config
```

Close confirmation is Ghostty's `confirm-close-surface` (default `true`: ask when a process is still running, including an agent). Set `false` to never ask, `always` to always ask.

Plugins only:

```toml
# ~/.config/ninja/ninja.toml
[plugins]
enabled = ["preview"]
```

`⌘,` opens the plugin panel. Toggle is start/stop.

## Plugins

Official examples live in **[ninja-plugins](https://github.com/chenglun17-0/ninja-plugins)**. The host crate never depends on them.

| Plugin | What it does |
| --- | --- |
| `preview` | ⌘-click a path → tab. Edit the file. ⌘S saves. |
| `agent-restore` | After window restore, resume `pi` / Codex / Claude in the right pane. |

```sh
# from ninja-plugins
./install-preview.sh
```

Write your own with the same protocol. If you need a new host API, two independent plugins should need it first.

## Protocol

Host and plugin are two processes. One frame = `u32le` length + UTF-8 JSON. `v` is the version. Unknown fields are ignored. Unknown plugin→host types are ignored. An unsupported `v` is not guessed.

```text
hit / layer / input / spawn / config / theme
```

Layers are `placement` × `surface` (`overlay` \| `side` \| `tab`) × (`pixels` \| `html`). The kernel has no plugin nouns.

## Documentation

| Doc | What it is |
| --- | --- |
| [PRODUCT.md](PRODUCT.md) | Product definition — what Ninja is and is not |
| [PLAN.md](PLAN.md) | Implementation contract: ownership, tech lock, quality gates |
| [DISTRIBUTION.md](DISTRIBUTION.md) | Packaging, signing, Gatekeeper |
| [docs/architecture.md](docs/architecture.md) | How the host and the ADE protocol compose |
| [docs/development.md](docs/development.md) | Checkout, build, test, package |
| [docs/cookbook/write-a-plugin.md](docs/cookbook/write-a-plugin.md) | Ship a process plugin, step by step |
| [`crates/ninja-protocol`](crates/ninja-protocol/src/lib.rs) | The wire contract: frames, `v`/`type`, message table, goldens |

## Status

First-year platform is macOS. Linux / Windows are not concurrent. Public distribution waits on a real Developer ID and notarization.

## Credits

Terminal core is [Ghostty](https://ghostty.org) via libghostty. Ninja is an application on that library, not a fork of the Ghostty app.
