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
  entry `<label> · <plugin>`. Denied, missing and invalid single operations return
  recoverable errors; a guest may handle them and commit its successful edits.
  Event failure, timeout, cancellation, revocation or stale document/surface roll
  back document and history. The adapter declares owner-thread authorization so
  state-dependent permission requirements are never evaluated on the worker.
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
- PhotoCraft-plugin-filter-gaps: done. Channels-panel colour subsets, Quick Mask, layer and
  vector masks, and non-pixel layers (smart objects included) disable plugin filters with a
  reason in the tooltip. Document profiles convert through CMS, so the `color` a plugin
  receives (`srgb` or `linear-srgb`) is the space its pixels are in.

## Plugin filters (done)

`photocraft_engine::plugin_filters` turns every enabled, contract-compatible effect into the
command `plugin.<plugin-id>/<contribution>` with a dialog, placed
in Filter › Plugins › <category> (default "Other"). The list follows store generation.
CLI and automation params JSON is the form Values object keyed by param key. Defaults are
filled, unknown keys and invalid types/ranges are refused. Choice/radio values are option ids,
colors are straight RGBA arrays (HDR RGB preserved), and points are `[x,y]` in the declared
layer-pixel or image-percent space. Integer values must be integral. No index, hex-color or
flattened-point aliases remain. Host command keys `layer`, `target`, `coalesce` and the `__` prefix are reserved.

Dialogs render `contribution.form` schema 1, optionally replaced by `craft_describe`, including
sections, ranges, integers, numbers, booleans, choice/radio, angle, color, point and multiline
text, units, help and host-evaluated visible/enabled conditions. Guest describe/update calls
run on workers in fresh sandboxes with event fuel, a 100 ms deadline and grant checks.
Each value change requests `craft_update`; only a result matching the current Values is applied.
Patches use the shared atomic revision validator and update values, hidden and disabled state.
Plugin presets accept single-effect recipes
`{"presets":[{"id":"print","label":"Print","effects":[{"effect":"plugin/effect","values":{}}]}]}`
and legacy direct `effect`/`values` records. The dialog lists only matching effects whose
Values validate; multi-effect recipes are omitted. Session `Node::Form` uses the
same widgets, condition evaluation and local edit state, emitting param-key events; unknown
`Node::Component` nodes recursively render their fallback.

A run is a normal
filter job: the active pixel layer's canvas area (RGB/RGBA 8/16/32) becomes premultiplied RGBA
f32 (`linear-srgb` for 32-bit, otherwise `srgb`), `FilterSource::run` executes it with
enablement/grant and cancellation checks, the result is unpremultiplied (clamped only for
integer depths), blended through the selection, locked transparency is kept, and the change is
one history step "<label> · <plugin name>". Errors leave the document unchanged and report
"<label> · <plugin> could not run: <reason>". Availability is checked when the menu is built and
again on the document thread before a result is applied; a result that no longer qualifies is
discarded with "<label> · <plugin> was discarded: <reason>". A disabled item's tooltip gives the
reason: a non-RGB document ("This filter works on RGB images"); a layer mask as the target or
shown in its mask view ("plugin filters can't edit a layer mask: target the layer's colour"); a
channel, channel subset or Quick Mask target ("plugin filters can't edit a channel or Quick Mask:
target the composite channel"); a profile that can't convert to sRGB ("This filter needs an sRGB
document"); a non-pixel layer. An RGB profile is converted in and out through CMS (to sRGB, or
linear sRGB for 32-bit documents). An explicit `target` argument is refused. Conversion and
parameter resolution live in `photocraft_plugins::filter` with unit tests.

## Plugin sessions: discovery and `filter.apply` (done)

Enabled session commands and panels, `plugin.session.<plugin>/<contribution>`, are command specs
(`photocraft_engine::plugin_sessions`) listed under Plugins › <plugin name> › <label> in menus,
`command.list`, `engine.commands`, the headless server and the CLI. `is_enabled` answers for them,
and they follow the store generation like plugin filters. Running one still goes through
`Session::execute`, which routes it to the session bridge.

`filter.apply`, a session's only way to change pixels, runs only the built-in pixel filters in
`photocraft_plugins::session_filters::FILTERS`: blur, sharpen, noise, distort, pixelate, the
render filters that take numbers only (clouds, fibers, lens flare, flame, tree), stylize, and the
Other and Video filters. Refused: filters that read a file, another document or another channel
(Displace, Lens Blur, Lighting Effects, Relight, Picture Frame); filters that run other commands
(Last Filter, Filter Gallery); Convert for Smart Filters; filters that open their own tool (Camera
Raw, Liquify, Vanishing Point, Lens Correction, Adaptive Wide Angle); the gallery and blur-gallery
filters for now; and every plugin and session command. Params must be an object of at most 16 KiB
with none of `target`, `layer`, `mapPath`, `mapDocument`, `mapLayer`, `depthChannel`, `depthMap`,
`texture`, or a key starting with `__`. A filter that isn't allowed is denied; bad params are
invalid.

## Catalogue, kill list and revocation (done)

The desktop app calls `photocraft_engine::plugin_sessions::start_catalogue_refresh` at startup.
That starts one background thread which refreshes the catalogue and kill list at once and every
six hours; a failed refresh is printed to stderr and tried again at the next interval. Sessions
don't start it, so tests and the CLI never reach the network. Every store update (install,
enable, grant, refresh, revocation) advances the store generation: the filter and session command
lists rebuild within two seconds, and session workers reload on their next pump. Until then each
run checks the store again (`FilterSource::validate`, `Runtime::valid`), so a kill-listed release
does not start. A filter invocation captures its grant generation when it starts and is rejected
if the generation changes at any host check (before the run, between fuel slices) or at commit.

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

Validated locally: `cargo test -p photocraft-plugins` (14 passed, including the
`session_filters` allowlist and argument tests); `cargo check` of `photocraft-plugins`,
`photocraft-engine` (with `--tests`), `photocraft-automation`, `photocraft-ui-egui` and
`photocraft`; and `cargo check -p photocraft-engine --target wasm32-unknown-unknown`.
`cargo check -p photocraft-automation --target wasm32-unknown-unknown` fails in the `errno`
dependency pulled in through `cap-std`, which this change does not touch. Added but not run
(engine tests are outside the local limit): `plugin_sessions::tests::filter_apply_runs_only_allowlisted_filters_with_clean_params`
and `plugin_filters::tests::plugin_filters_say_why_they_are_unavailable_on_masks_channels_and_profiles`.
No build/run, workspace tests or clippy were invoked.

## Suite-review parity

Tools are not supported in PhotoCraft yet. The provided contract marks Tool
contributions unavailable with that reason, even when their session contract matches.
Native helpers are supervised on startup and at least once per second while idle;
revocation, lost consent, safe mode or an unreadable grant store stops them.
Live grants open a fresh store snapshot for every check: read errors deny permissions,
report stopped state and invalidate running invocation generations until readable again.

Parity-fix validation: `cargo test -p photocraft-plugins` passed all 16 tests,
including recoverable bridge errors followed by another call and commit, owner-thread
opt-in and Tool unavailability. `cargo check -p photocraft-engine` passed.
The idle native-helper cadence was inspected in code; no helper/app was launched.

## Form schema v1 validation

The form migration removes the previous parameter representation without compatibility aliases.
Visual/runtime verification was not run under the no-build/no-run task limits.

Form migration checks: `cargo check -p photocraft-plugins`, `cargo check -p photocraft-ui-egui`
and `cargo test -p photocraft-plugins` (16 passed). Tests cover typed Values, rejected old aliases,
HDR/point preservation and effect-scoped preset decoding.
