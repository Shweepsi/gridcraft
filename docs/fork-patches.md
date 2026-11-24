# Fork patches (Shweepsi/gridcraft, branch `shared-cells-*`)

What this fork carries on top of upstream's tag, so a version bump knows what to
re-apply and what to drop. Last checked against `v0.19.1` (24/11/2026).

Method: restart from the new upstream tag, apply the *product layer* as is, then
for each *generic patch* check whether upstream has it now; drop ours if so.

## Product layer — ours, in files upstream doesn't have (no conflicts)

| What | Files |
|---|---|
| Shared workbooks: cell diffs, LWW by `seq` | `crates/engine/src/collab.rs`, `apps/gridcraft-web/src/shared.rs` |
| Page embedding: `?host=parent` messages (`gridcraft:open/changes/apply/theme/select`) | `apps/gridcraft-web/src/embed.rs` |
| MCP engine as a Worker | `apps/gridcraft-worker/`, `packaging/worker/package.sh`, `docs/mcp.md` |
| French labels the Suivi workbook needs | `crates/ui-egui/src/i18n/fr_suivi.rs` (asked after upstream's `fr.rs`) |
| Builds | `.github/workflows/web.yml`, `worker.yml`, `packaging/web/package.sh`, `renderer-glow` feature |

## Hooks into upstream files — small, the only real conflict sources

`DocState.shared`, `Ctx.arrays_checked` (`cmd.rs`), recalculation after undo/redo
(`cmd/edit.rs`), exports in `engine/src/lib.rs` and `ui-egui/src/lib.rs`, wiring in
`apps/gridcraft-web/src/{web,host,main}.rs`, `xtask/src/{layers,main}.rs`, `Cargo.toml`.

## Generic patches — drop ours when upstream has the same

| Patch | Files | Upstream to watch |
|---|---|---|
| Conditional formats kept only in the x14 extension | `xlsx/src/sheet_read.rs` (+`rules.rs`, `styles.rs` visibility), test in `tests/fixtures.rs` | PR #2262 (open, conflicting), #781 |
| Typing in a drop-down list: suggestions, accent-blind, word-prefix, enters the entry meant | `engine/src/cmd/data/validation.rs`, `ui-egui/src/{editor,formula_bar,app/editing}.rs`, `tests/validation_list_typing.rs` | PR #2119 (merged 23/11, after v0.19.1): compare, keep only what it lacks |
| Date entry (`12/3`, month names, short years) | `core/src/parse.rs`, `engine/src/{display,cmd/edit/cell}.rs`, `ui-egui/src/keymap.rs` | #3106 (day-first) overlaps; not the same |
| Ribbon: `?tabs=` filter, title bar extracted from the ribbon | `ui-egui/src/ribbon.rs`, `ribbon/title_bar.rs` | none found |
| Web font loader | `ui-egui/src/theme.rs`, `theme/font_setup.rs` | none found |
| Reveal the selection centred | `ui-egui/src/grid.rs`, `grid/show.rs` | none found |

## Not ported from the previous fork version

Conditional-format performance patch (upstream rewrote `cf.rs`, no slowness seen).
A `glow` (WebGL2-only) build exists as a CI matrix leg but isn't what Working Book pins.
