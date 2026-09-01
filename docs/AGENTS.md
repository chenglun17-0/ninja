# Documentation standard

Human-facing docs describe **current** behavior. Change stories live in git commits, not here.

## Tiers — one home per fact

| Home | Job | Does not belong there |
| --- | --- | --- |
| [PRODUCT.md](../PRODUCT.md) | What Ninja is and is not | Build steps, wire fields, crate layout |
| [PLAN.md](../PLAN.md) | Implementation contract: ownership, tech lock, tree, deferred work | Sprint evidence, phase diaries |
| [README.md](../README.md) | GitHub-facing: install, configure, what a plugin is | Protocol schema |
| [DISTRIBUTION.md](../DISTRIBUTION.md) | Packaging, signing, Gatekeeper | Plugin authoring |
| [architecture.md](architecture.md) | How the host and ADE compose | Per-message fields, contributor setup |
| [development.md](development.md) | Checkout, build, test, package | Product rationale |
| [cookbook/write-a-plugin.md](cookbook/write-a-plugin.md) | Numbered steps to ship a process plugin | New host APIs |
| `crates/ninja-protocol` | Wire contract: frames, `v`/`type`, message table, goldens | Host adapter bugs |
| Root [AGENTS.md](../AGENTS.md) | Standing orders for agents | Worked examples |

Do not add a third product contract. Do not duplicate the protocol table outside `ninja-protocol`. Do not keep sprint evidence under `docs/`.

## Writing

- Name the live mechanism. Avoid "previously / now / q3 / 取证".
- One fact, one home; elsewhere, link.
- Tutorials (cookbook) follow an ordered path. References (architecture, protocol crate) do not teach.
- English or Chinese is fine; do not maintain a bilingual pair unless the user asks.
