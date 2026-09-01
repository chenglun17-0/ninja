# Agent notes — ninja

This repository is the **Ninja host** (kernel). It is a macOS GPU terminal on libghostty, plus a narrow ADE plugin protocol. It is not an IDE, not an agent product, and not a plugin marketplace.

Product contract: [PRODUCT.md](PRODUCT.md). Implementation contract: [PLAN.md](PLAN.md). GitHub-facing copy: [README.md](README.md).

## Do

- Keep the empty-load app a complete terminal (windows, tabs, splits, Ghostty config).
- Put Ghostty semantics in the host adapter, never in the protocol.
- Add protocol primitives only when existing ones cannot compose the feature — and preferably when a second independent plugin needs them.
- Keep plugin nouns out of the kernel (`preview`, `editor`, `save`, `git`, `lsp`).

## Don't

- Don't link plugins into the host. Don't ship plugins in the .app.
- Don't add WK/WebKit APIs for a single plugin. HTML layers are load-document + opaque `layer.msg`.
- Don't maintain a second terminal keymap. `~/.config/ghostty/config` is the terminal config; `ninja.toml` is plugins only.

## Git commits

Write **commit messages in English**.

- Imperative subject, ~50–72 characters: `feat: …`, `fix: …`, `docs: …`, `refactor: …`.
- Body (optional) explains *why*, not a file list. Wrap at 72.
- One logical change per commit when practical.
- Do not commit `dist/`, secrets, or generated `target/`.

Examples:

```text
feat: split layer.open into placement and surface

Tab layers can be pixels or HTML. The host does not infer preview vs editor.
```

```text
fix: keep search field focused after clicking next/prev
```
