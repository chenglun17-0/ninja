# Write a plugin

A Ninja plugin is a separate process. It never links the host and never links `ghostty.h`; it speaks JSON frames over a Unix socket. The wire contract (frames, versioning, every message) lives in [`crates/ninja-protocol`](../../crates/ninja-protocol/src/lib.rs) — this page is the path, not a second spec.

Reference implementations: [ninja-plugins](https://github.com/chenglun17-0/ninja-plugins) (`preview` is the smallest full example: claim hits, open a tab layer, edit, save).

## 1. Decide what the plugin claims

You get seven message families: `hit` / `layer` / `input` / `spawn` / `config` / `theme` / `pane`. Most UI plugins start from two:

- `hit` — the terminal clicked a path / URL / OSC-8 link. Claim it or ignore it.
- `layer` — ask for a surface: `placement` (`overlay` / `side` / `tab`) × `surface` (`pixels` / `html`).

If you need a new host API for your feature, stop: a second independent plugin must need it first.

## 2. Speak the protocol from your language

- One frame = `u32le` length prefix + UTF-8 JSON (`{"v":0,"type":"...",...}`).
- `v` mismatch → exit with code 78. Never guess an older format.
- Unknown `type` from the host → decode error is fine to log and drop; unknown fields are ignored.
- Golden JSON for every message: `crates/ninja-protocol/tests/golden/`. A second-language decoder only needs this doc plus those files (`tests/second_language_decode.py` is a minimal Python proof).

In Rust, depend on `ninja-protocol` and use `Message`, `FrameDecoder`, `encode_frame` — nothing else.

## 3. Write the process loop

```text
read NINJA_ADE_SOCK from env        # the host sets it when spawning you
connect with retry (~5s)            # the host may still be binding
loop:
  read socket → feed FrameDecoder → pop frames
  decode_plugin(frame)
    UnsupportedVersion → stderr one line, exit 78
    other decode error → log, continue
  dispatch by type:
    Hit   → text is a path you handle? send hit.claim(id, priority) : hit.ignore(id)
    LayerReady → you now own a surface; pixels: draw into the IOSurface id,
                 html: send layer.html with a full document
    ...   → unknown types: ignore
EOF → exit 0                        # host closed: normal shutdown, no cleanup needed
```

The host kills nothing on the happy path. Socket EOF is your shutdown signal.

Hit payloads carry two details the message table implies but plugins trip on:

- `text` may carry `:line` / `:line:col` suffixes (`src/main.rs:42:13`) — strip
  them before treating the rest as a path.
- a relative path resolves against `cwd`, which can arrive as a `file://` URL —
  strip the scheme before joining.

Both sides send `layer.open` with `id` = the hit id right after `hit.claim`
(back to back); the host digests it during the claim window and echoes the id
in `layer.ready`, which is how you pair the surface with the file you opened.

## 4. Install and toggle

```sh
cargo build --release              # your plugin binary, any name without '/'
cp target/release/my-plugin ~/.config/ninja/plugins/my-plugin
```

Open the plugin panel (`⌘,`). The binary appears as an unchecked **未启用** row. Toggle it on: the host starts the process immediately and writes the name into `ninja.toml`. Toggle off: process killed, layers closed, theme override revoked, socket removed when the list empties.

Replacing the binary while enabled restarts the process (mtime watch). No host restart needed for any of this.

## 5. Verify

1. Panel: toggle on → row shows `pid <n> · <mb> MB`; toggle off → row returns to 未启用, no `my-plugin` process remains (`pgrep -f my-plugin` empty).
2. Crash isolation: `kill -9` the plugin mid-layer → layer closes, terminal keeps running, host stays up.
3. Version gate: hand-connect with `{"v":99,...}` → the host logs and drops the connection; the host itself never exits.
4. Empty load: with your plugin disabled, no `ninja-ade-*.sock` for the host pid and no plugin processes.

If any of these fail, the bug is in the host supervisor or your protocol handling — not something to patch around in `ninja.toml`.
