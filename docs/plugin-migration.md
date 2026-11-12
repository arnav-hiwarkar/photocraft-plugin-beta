# Shared plugin adapter migration

The former loose-Wasm filter ABI, registry, examples, commands, preference folder
loader and menu renderer have been removed. `photocraft-plugins` now depends on the
shared Craft core/host/store. Desktop management uses the shared ManagerView;
`photocraft-cli plugin …` uses the shared CLI, including a local `contract` command.
Safe mode leaves management available and refuses executable discovery/transforms.

The adapter provides enabled effect discovery, isolated shared-host transforms,
live enabled/grant checks and a bounded worker/document request/reply channel.
It returns transformed pixels only on success. Its input is premultiplied RGBA f32;
callers must supply the true document working-space name.

The following named integration tasks remain; this migration is **incomplete**:

- PhotoCraft-plugin-transactions: done. Enabled `photocraft.session@1` command,
  panel and event-hook contributions use ordered Wasm workers (one instance per
  plugin/module) or a consented native helper. `DocumentApi` requests are served
  only on the engine/document thread. Each event captures document id, activation
  epoch and revision **at dispatch**; every operation rechecks contribution enablement,
  live grants and `authorize()` immediately before execution. Begin → calls → finish
  are served without interleaving user edits. Success folds edits into one history
  entry `<label> · <plugin>`; errors (including ignored operation errors), timeout,
  cancellation, revocation or stale document/surface roll back document and history.
  Commands execute synchronously through `Session::execute` (also for CLI/automation).
- PhotoCraft-plugin-panels: done. Shared Nodes are validated before publication and
  drawn by ui-egui, including panels and `ui.window.open` floating windows. UI changes
  publish only on event commit. Widget events retain surface revisions and are
  checked again when serviced; sliders and text fields retain per-widget edit state.
  Commands appear in Plugins ▸ <plugin> ▸ <label>. `selection.changed` (layer or
  pixel selection) and `document.opened` go to event hooks declaring and holding
  `app.events`. The desktop frame pumps requests; safe mode loads no workers.
- PhotoCraft-plugin-projects: not needed for the current destructive plugin filters.
  Plugin filters accept raster layers only (`plugin_filters::enabled` and the run
  path both enforce this); PhotoCraft's existing smart filters store built-in filter
  command ids, not plugin contributions. Destructive pixels are self-contained in
  native documents and export needs no plugin. Revisit persistence/missing-plugin
  export refusal only when non-destructive plugin smart filters are introduced.
- TODO(PhotoCraft-plugin-examples): verify unchanged halftone and film-looks packages
  end to end in the running app (filter wiring exists; not yet exercised on real packages).
- TODO(PhotoCraft-plugin-filter-gaps): Channels-panel colour subsets, layer/filter-mask
  targets and smart objects are not offered to plugin filters (the command is disabled or
  edits the layer's composite colour); non-sRGB integer documents are labelled `srgb`.

## Plugin filters (done)

`photocraft_engine::plugin_filters` turns every enabled, contract-compatible effect into the
command `plugin.<plugin-id>/<contribution>` (label ends in "…" when it has parameters), placed
in Filter › Plugins › <category> (default "Other"). The list is rebuilt when the store
generation changes, checked at most every two seconds; `command.list`, `engine.commands`, the
CLI and automation run these ids with a params object like any built-in filter. Contributions
declaring a parameter named `layer`, `target` or `coalesce` are not offered (the host reads
those keys). Params: slider numbers within range (integer sliders round), angle degrees,
checkbox bool, select option name or index, color `[r,g,b,a]`/`#rrggbb[aa]`, point `[x,y]`
(or `<id>.x`/`<id>.y`), text ≤ 4096 bytes; every declared parameter reaches the guest, with
defaults filled in and unknown keys refused.

The generated filter dialog shows manifest labels, angle dials, raw option names, a Preview
toggle and a Preset menu listing that filter's single-effect plugin presets. A run is a normal
filter job: the active pixel layer's canvas area (RGB/RGBA 8/16/32) becomes premultiplied RGBA
f32 (`linear-srgb` for 32-bit, otherwise `srgb`), `FilterSource::run` executes it with
enablement/grant and cancellation checks, the result is unpremultiplied (clamped only for
integer depths), blended through the selection, locked transparency is kept, and the change is
one history step "<label> · <plugin name>". Errors leave the document unchanged and report
"<label> · <plugin> could not run: <reason>". In non-RGB documents the menu item is disabled
with the tooltip "This filter works on RGB images". Conversion and parameter resolution live
in `photocraft_plugins::filter` with unit tests.

Both Color Lookup pickers also offer plugin LUTs, labelled "From plugins", using their
existing asynchronous scan.

The engine dispatches the adapter contract operations through the session bridge. Shared source is read-only except
for the PhotoCraft contract page.

`plugin.resources` returns an array of `{plugin, id, label, kind, files}` records;
`kind` is `preset` or `lut`, and `files` contains canonical package-contained paths.
Safe mode and the web adapter return an empty array. Discovery re-reads the enabled
store view, including kill-list and required-grant checks. LUT menus refresh on the
existing two-second asynchronous scan cadence; failures leave that scan empty.

## Validation

Scoped crate checks and `cargo test -p photocraft-plugins` cover the adapter/bridge.
The bridge regressions cover independent worker document captures, timeout cancellation
with rollback delivery, commit rejection propagation, and panel validation/revisions.
Engine regressions also cover multi-op history folding and complete rollback (added,
not executed under the plugin-only test limit). Runtime package/UI exercise
and broader engine/UI tests remain unrun under the task’s local validation limits.

Validated locally: `cargo test -p photocraft-plugins` (9 passed),
`cargo check -p photocraft-plugins`, `cargo check -p photocraft-engine`,
and the final `cargo check -p photocraft-ui-egui` (including engine and plugin
dependencies). No build/run, workspace tests or clippy were invoked.
