//! PhotoCraft's plugin contract: every operation a session plugin can call, the permission each
//! needs for its real arguments, the events it receives and where contributions appear.
//!
//! Permission rules are total and conservative: they never trust a flag the plugin passes to lower
//! what a call needs. Operations that are not listed here are unreachable (no undo, history edits,
//! preferences, plugin management, raw files, shell or processes).
use craft_ext_core::{
    contract::Provided,
    id::{AppId, IdError},
    permission::{DocScope, Permission},
};
use craft_ext_host::api::{AppContract, EventSpec, OpKind, OpSpec, SlotSpec, none};
use serde_json::Value;

/// The session contract level this build provides. 1.1 carries UI level 1.1 (the shared renderer),
/// tools, overlays, pixels, layers, selection, guides, histogram, adjustments and picked files.
pub const SESSION_LEVEL: (u32, u32) = (1, 1);

fn read_document(_: &Value) -> Vec<Permission> {
    vec![Permission::DocumentRead(DocScope::Document)]
}
fn read_all(_: &Value) -> Vec<Permission> {
    vec![Permission::DocumentRead(DocScope::All)]
}
fn read_selection(_: &Value) -> Vec<Permission> {
    vec![Permission::DocumentRead(DocScope::Selection)]
}
fn edit_document(_: &Value) -> Vec<Permission> {
    vec![Permission::DocumentEdit(DocScope::Document)]
}
fn window(_: &Value) -> Vec<Permission> {
    vec![Permission::UiWindow]
}
/// Opening a file needs `files.read:picked`; every other pick (a folder or a file to save) is for
/// writing. An unknown mode asks for both, and the operation then refuses it.
fn pick(args: &Value) -> Vec<Permission> {
    match args.get("mode").and_then(Value::as_str) {
        Some("open") => vec![Permission::FilesRead],
        Some("folder" | "save") => vec![Permission::FilesWrite],
        _ => vec![Permission::FilesRead, Permission::FilesWrite],
    }
}
fn files_write(_: &Value) -> Vec<Permission> {
    vec![Permission::FilesWrite]
}
fn files_read(_: &Value) -> Vec<Permission> {
    vec![Permission::FilesRead]
}
fn export(_: &Value) -> Vec<Permission> {
    vec![Permission::FilesWrite, Permission::DocumentRead(DocScope::Document)]
}
fn import(_: &Value) -> Vec<Permission> {
    vec![Permission::FilesRead, Permission::DocumentEdit(DocScope::Document)]
}

type Rule = fn(&Value) -> Vec<Permission>;

/// (id, kind, rule, rule text, summary, args, result).
const OPS: &[(&str, OpKind, Rule, &str, &str, &str, &str)] = &[
    (
        "document.info",
        OpKind::Query,
        read_document,
        "document.read:document",
        "The active document.",
        "{}",
        r#"{"id": 3, "name": "photo.jpg", "width": 1920, "height": 1080, "mode": "rgb", "depth": 8, "resolution": 72, "revision": 12, "active_layer": 7, "layer_count": 4, "has_selection": false}"#,
    ),
    (
        "documents.list",
        OpKind::Query,
        read_all,
        "document.read:all",
        "Every open document (metadata only); `active` marks the one plugin operations act on.",
        "{}",
        r#"[{"id": 3, "name": "photo.jpg", "width": 1920, "height": 1080, "active": true}]"#,
    ),
    (
        "layer.list",
        OpKind::Query,
        read_document,
        "document.read:document",
        "The layers, top to bottom, with groups expanded (`parent` names the group; at most 64 levels).",
        "{}",
        r#"[{"id": 7, "name": "Sky", "kind": "pixel", "parent": null, "visible": true, "opacity": 100, "blend": "normal", "bounds": [0, 0, 1920, 600], "has_mask": false, "clipped": false, "locked": false}]"#,
    ),
    (
        "layer.get",
        OpKind::Query,
        read_document,
        "document.read:document",
        "One layer (default: the active layer), with the fields of `layer.list`.",
        r#"{"layer": 7}"#,
        r#"{"id": 7, "name": "Sky", "kind": "pixel", …}"#,
    ),
    (
        "selection.get",
        OpKind::Query,
        read_selection,
        "document.read:selection",
        "The layer selection and the pixel selection's bounds, captured when the event started.",
        "{}",
        r#"{"layers": [7], "active": 7, "has_selection": true, "bounds": [10, 10, 200, 120]}"#,
    ),
    (
        "selection.mask.get",
        OpKind::Query,
        read_selection,
        "document.read:selection",
        "The pixel selection's coverage as 8-bit gray (255 = selected), base64. `rect` defaults to the whole image. At most 4 Mi pixels.",
        r#"{"rect": [0, 0, 256, 256]}"#,
        r#"{"x": 0, "y": 0, "width": 256, "height": 256, "format": "gray8", "data": "AAAA…"}"#,
    ),
    (
        "pixels.get",
        OpKind::Query,
        read_document,
        "document.read:document",
        "Pixels of a pixel layer (default: the active layer) or, with `composite: true`, of the visible image. `rect` [x, y, width, height] in image pixels defaults to the whole image and is clipped to it. Straight RGBA in the document's own values; `format` `rgba8` (default), `rgba16` (little-endian u16) or `rgba32f` (little-endian f32, linear light in 32-bit documents). `data` is base64. At most 4 MiB of pixel data per call. RGB documents only.",
        r#"{"layer": 7, "rect": [0, 0, 64, 64], "format": "rgba8"}"#,
        r#"{"x": 0, "y": 0, "width": 64, "height": 64, "format": "rgba8", "data": "…"}"#,
    ),
    (
        "histogram.get",
        OpKind::Query,
        read_document,
        "document.read:document",
        "256-bin histograms of a pixel layer (default: the active layer) or the visible image (`composite: true`). `rect` limits the area; the pixel selection limits it too unless `selection: false`. Transparent pixels are not counted. Luminosity is Rec. 709. Computed in bounded tiles; at most 64 Mi pixels per call (pass a smaller `rect` for larger images).",
        r#"{"composite": true, "selection": true}"#,
        r#"{"count": 2073600, "red": [256 counts], "green": [...], "blue": [...], "luminosity": [...], "mean": {"red": 118.2, "green": 120.9, "blue": 99.1, "luminosity": 117.4}}"#,
    ),
    (
        "guides.list",
        OpKind::Query,
        read_document,
        "document.read:document",
        "The guides in image pixels.",
        "{}",
        r#"{"horizontal": [100.0], "vertical": [960.0]}"#,
    ),
    (
        "channels.list",
        OpKind::Query,
        read_document,
        "document.read:document",
        "The colour channels and saved alpha channels.",
        "{}",
        r#"[{"index": 0, "name": "Red", "kind": "color"}, {"index": 3, "name": "Alpha 1", "kind": "alpha"}]"#,
    ),
    (
        "history.list",
        OpKind::Query,
        read_document,
        "document.read:document",
        "The undo history labels, oldest first; `current` counts the steps that can be undone.",
        "{}",
        r#"{"entries": ["Open", "Levels"], "current": 2}"#,
    ),
    (
        "tool.state",
        OpKind::Query,
        none,
        "free",
        "The tool gesture under way (the last `tool` event's payload), or null.",
        "{}",
        r#"{"tool": "gradient", "phase": "update", "at": [120.5, 40.0], "start": [10.0, 10.0], "modifiers": {"shift": true}, "zoom": 0.5}"#,
    ),
    (
        "layer.rename",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Rename a layer (default: the active layer). Names are at most 4096 bytes.",
        r#"{"layer": 7, "name": "Sky"}"#,
        "null",
    ),
    (
        "layer.create",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Add a layer above the active one and make it active: `pixel` (empty), `group`, or `fill` (a solid colour, straight RGBA 0–1). At most 100 per event.",
        r#"{"kind": "pixel", "name": "Gradient", "opacity": 100, "blend": "normal"}"#,
        r#"{"id": 9}"#,
    ),
    ("layer.delete", OpKind::Edit, edit_document, "document.edit:document", "Delete a layer.", r#"{"layer": 9}"#, "null"),
    (
        "layer.set",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Change a layer's name, visibility, opacity or fill opacity (0–100) or blend mode (`normal`, `multiply`, `screen`, `overlay`, `soft-light`, `hard-light`, `color-dodge`, `color-burn`, `darken`, `lighten`, `difference`, `exclusion`, `hue`, `saturation`, `color`, `luminosity`).",
        r#"{"layer": 9, "visible": true, "opacity": 60, "blend": "multiply"}"#,
        "null",
    ),
    ("layer.select", OpKind::Edit, read_document, "document.read:document", "Make a layer the active (and only selected) layer.", r#"{"layer": 9}"#, "null"),
    (
        "pixels.set",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Write straight RGBA pixels into a pixel layer (default: the active layer) at [x, y]. `format` and `data` as `pixels.get`; the area must lie inside the image. With `selection: true` the pixels blend in through the pixel selection. Locked layers are refused; locked transparency keeps alpha. Arguments are at most 1 MiB, so write large images in tiles (one event is still one undo step).",
        r#"{"layer": 9, "x": 0, "y": 0, "width": 64, "height": 64, "format": "rgba8", "data": "…", "selection": false}"#,
        "null",
    ),
    (
        "selection.set",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Change the pixel selection: `{\"all\": true}`, `{\"none\": true}`, a `rect` [x, y, width, height], an `ellipse` [x, y, width, height], or a `mask` (gray8 base64 with x, y, width, height). `mode` is `replace` (default), `add`, `subtract` or `intersect`.",
        r#"{"rect": [10, 10, 100, 50], "mode": "add"}"#,
        "null",
    ),
    (
        "selection.save",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Save the pixel selection as a new alpha channel.",
        r#"{"name": "Sky mask"}"#,
        "null",
    ),
    (
        "guide.add",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Add a guide at a position in image pixels (within ten image sizes of the canvas; at most 1000 guides).",
        r#"{"orientation": "vertical", "position": 960}"#,
        "null",
    ),
    ("guides.clear", OpKind::Edit, edit_document, "document.edit:document", "Remove every guide.", "{}", "null"),
    (
        "mask.add",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Add a layer mask to a layer (default: the active layer): `reveal` is `all`, `none` or `selection`.",
        r#"{"layer": 7, "reveal": "selection"}"#,
        "null",
    ),
    (
        "filter.apply",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Run an allowlisted built-in pixel filter on the active layer: `{filter, params}`. Filters that read files, other documents or channels, or run other commands, are refused.",
        r#"{"filter": "filter.blur.gaussianBlur", "params": {"radius": 4}}"#,
        "filter result",
    ),
    (
        "adjustment.apply",
        OpKind::Edit,
        edit_document,
        "document.edit:document",
        "Apply a built-in adjustment to the active layer, or add it as an adjustment layer with `as_layer: true`: brightnessContrast, levels, curves, exposure, vibrance, hueSaturation, colorBalance, blackWhite, photoFilter, channelMixer, invert, posterize, threshold, gradientMap, selectiveColor; destructive only: desaturate, equalize. `params` as the adjustment's command (at most 16 KiB).",
        r#"{"adjustment": "levels", "params": {"inputBlack": 10, "inputWhite": 240}, "as_layer": true}"#,
        "adjustment result",
    ),
    (
        "files.pick",
        OpKind::Effect,
        pick,
        "`files.read:picked` for `open`; `files.write:picked` for `folder` and `save`",
        "Ask the person to pick a folder (`folder`), a file to write (`save`, with a suggested `name`) or an image to read (`open`). Returns at once with a `request` number; the answer arrives later as a `files.picked` event with an opaque `handle` (or `cancelled`). Handles last until the app quits and only work for the plugin that asked. Headless: `photocraft-cli run --pick <PATH>` answers in order.",
        r#"{"mode": "folder", "title": "Export layers to"}"#,
        r#"{"request": 1}"#,
    ),
    (
        "files.write",
        OpKind::Effect,
        files_write,
        "files.write:picked",
        "Write a small file (≤ 1 MiB, `text` or base64 `data`) to a picked save file, or into a picked folder as `name` (a plain file name; hidden and executable names are refused).",
        r#"{"handle": "h1", "name": "layers.json", "text": "{…}"}"#,
        r#"{"bytes": 1234}"#,
    ),
    (
        "files.read",
        OpKind::Effect,
        files_read,
        "files.read:picked",
        "Read a picked file (at most 4 MiB), base64.",
        r#"{"handle": "h2"}"#,
        r#"{"name": "notes.txt", "bytes": 120, "data": "…"}"#,
    ),
    (
        "document.export",
        OpKind::Effect,
        export,
        "files.write:picked and document.read:document",
        "Export the image, or one layer (`layer`), as an image file: to a picked save file, or into a picked folder as `name`. The format follows the extension: png, jpg, jpeg, tif, tiff, webp or psd. At most 500 exports per event.",
        r#"{"handle": "h1", "name": "Sky.png", "layer": 7}"#,
        r#"{"bytes": 52311, "width": 1920, "height": 600}"#,
    ),
    (
        "layer.import",
        OpKind::Edit,
        import,
        "files.read:picked and document.edit:document",
        "Place a picked image file as a new embedded layer.",
        r#"{"handle": "h2"}"#,
        "place result",
    ),
    (
        "ui.panel.set",
        OpKind::Ui,
        none,
        "free",
        "Draw a panel your manifest declares (UI level 1.1); published when the event commits.",
        r#"{"panel": "levels", "tree": {…}}"#,
        r#"{"rev": 4}"#,
    ),
    (
        "ui.window.open",
        OpKind::Ui,
        window,
        "ui.window",
        "Open or redraw a floating window (at most 32 surfaces per app).",
        r#"{"id": "about", "title": "About", "tree": {…}}"#,
        r#"{"rev": 5}"#,
    ),
    ("ui.notify", OpKind::Ui, none, "free", "Show a short notification (at most 32 queued, 4000 characters).", r#"{"message": "Exported 4 layers"}"#, "null"),
    (
        "canvas.overlay.set",
        OpKind::Ui,
        none,
        "free",
        "Draw guides over the canvas: a UI draw list (no images) whose coordinates are image pixels; stroke widths and text sizes are screen points. Published when the event commits, cleared when a tool gesture ends. At most 20 000 ops and 1 MiB.",
        r#"{"ops": [{"op": "line", "from": [0, 0], "to": [100, 100], "width": 1, "rgba": [1, 0, 0, 1]}]}"#,
        "null",
    ),
    (
        "plugin.resources",
        OpKind::Query,
        none,
        "free",
        "The enabled presets and LUTs of every plugin.",
        "{}",
        r#"[{"plugin": "…", "id": "film-warm", "label": "Film warm", "kind": "lut", "files": ["…"]}]"#,
    ),
];

/// Fallible because the shared platform validates app identifiers at construction.
pub fn contract() -> Result<AppContract, IdError> {
    Ok(AppContract {
        app: AppId::new("photocraft")?,
        display_name: "PhotoCraft",
        provided: Provided::new(&[
            ("raster.filter", 1, 0),
            ("photocraft.session", SESSION_LEVEL.0, SESSION_LEVEL.1),
            ("resource.preset", 1, 0),
            ("resource.lut", 1, 0),
            ("craft-theme", 1, 0),
        ]),
        ops: OPS
            .iter()
            .map(|&(id, kind, requires, permission_rule, summary, args, result)| OpSpec { id, kind, summary, args, result, requires, permission_rule })
            .collect(),
        events: [
            ("init", "{\"event\":\"init\",\"contribution\"}", false, "After the plugin loads and whenever plugins reload; draw panels here"),
            ("command", "{\"event\":\"command\",\"id\",\"params\"}", false, "A command contribution ran (menu, `photocraft-cli run --cmd`, automation)"),
            ("ui", "{\"event\":\"ui\",\"surface\",\"rev\",\"widget\",\"value\"}", false, "A panel or window widget changed (see the UI reference for values)"),
            (
                "tool",
                "{\"event\":\"tool\",\"tool\",\"phase\":\"begin|update|commit|cancel\",\"at\":[x,y],\"start\":[x,y],\"modifiers\":{shift,alt,ctrl,cmd},\"button\",\"pressure\",\"zoom\"}",
                false,
                "A gesture of your selected tool, in image pixels; `zoom` is screen points per image pixel. Escape sends `cancel`",
            ),
            ("files.picked", "{\"event\":\"files.picked\",\"request\",\"handle\",\"name\"} or {…,\"cancelled\":true}", false, "The answer to `files.pick`"),
            ("app", "{\"event\":\"app\",\"name\",\"payload\"}", true, "document.opened, selection.changed (layers or pixels)"),
            ("dispose", "{\"event\":\"dispose\"}", false, "The worker is stopping"),
        ]
        .into_iter()
        .map(|(name, payload, needs_app_events, summary)| EventSpec { name, summary, payload, needs_app_events })
        .collect(),
        slots: vec![
            SlotSpec { kind: "effect", summary: "Filter ▸ Plugins ▸ category" },
            SlotSpec { kind: "command", summary: "Plugins ▸ plugin ▸ label" },
            SlotSpec { kind: "panel", summary: "A floating plugin panel, toggled by Plugins ▸ plugin ▸ label" },
            SlotSpec { kind: "tool", summary: "Plugins ▸ plugin ▸ label selects it; the canvas sends its gestures" },
            SlotSpec { kind: "event-hook", summary: "App event subscriptions (needs `app.events`)" },
            SlotSpec { kind: "preset", summary: "Plugin filter presets" },
            SlotSpec { kind: "lut", summary: "Color Lookup ▸ From plugins, `plugin.resources`" },
        ],
        notes: NOTES,
    })
}

const NOTES: &str = r#"Each session event is one transaction and one undo step labelled "<label> · <plugin>"; an event that fails, times out, is revoked or finds the document changed rolls back. A tool gesture (begin … commit) is one undo step; Escape, a failed event, a document switch or revocation mid-gesture rolls the whole gesture back. Coordinates are image pixels (fractional; the top-left pixel spans 0–1). While a plugin tool is selected the canvas still zooms and pans (wheel, pinch, Space, middle button, the Zoom keys); choosing any built-in tool deselects it. From the CLI a tool command takes `{"points": [[x, y], …]}` (begin at the first point, commit at the last; `"cancel": true` cancels instead). A plugin whose events fail three times in a row is paused; the Plugins window shows it with Resume, as does the engine command `plugin.resume {"plugin": id}` (pauses last for one process).

Pixel operations work on RGB documents (8, 16 and 32 bit). Pixel data is straight (not premultiplied) RGBA in the document's own values, base64-encoded; a 32-bit document's values are linear light. Plugin filters, by contrast, receive premultiplied RGBA f32 in sRGB for integer depths or linear sRGB for float, with CMS conversion. Other modes are disabled with a reason. Filters apply destructively as one history step; plugin smart filters are unsupported.

Picked files: file operations (`files.*`, `document.export`, `layer.import`) are refused during tool gestures because document rollback cannot undo external I/O. A plugin never sees a path. `files.pick` returns a request number, and the person's answer arrives as a `files.picked` event with an opaque handle that only that plugin can use, for that file or folder only, until the app quits. Writes are staged until event commit, then published in order using per-file atomic replacement without following a target symlink. On the first write failure, commit returns an error and document/UI changes roll back; earlier published files remain and the error reports their count. The failing destination may also have changed if a final durability flush failed. This is not a multi-file atomic transaction. Hidden and executable save names are refused for both save and folder handles. Plugins get no raw file system, network, shell or process access; native plugins (full access, separate consent) are the only way to get it.

Effect dialogs use Form schema 1 (`contribution.form`, optional worker `craft_describe`). Filter context `params` and CLI/automation parameters are Values keyed by param key: choice/radio option ids, straight RGBA including HDR RGB, and `[x,y]` points in their declared space. Defaults are filled; unknown keys and invalid values are refused. `layer`, `target`, `coalesce` and the `__` prefix are host-reserved command keys.

The host draws all form kinds, units, help and sections, evaluates visible/enabled conditions, and calls optional `craft_update` on workers after edits. Event fuel, grant checks and a 100 ms deadline bound form calls. Patches apply atomically at the current revision; stale edit results are discarded. Plugin presets accept `{id,label,effects:[{effect,values}]}` single-effect recipes and legacy direct `{id,label,effect,values}` records inside a `presets` array; the dialog offers matching, validated presets and omits multi-effect recipes.

Panels, windows and filter dialogs are drawn by the shared renderer (`craft_ext_ui::plugin_ui`, UI level 1.1) with the engine chosen in Preferences ▸ Plugins (egui by default); plugins cannot choose or detect it. A contribution declaring `photocraft.session@1.0` may only send level-1.0 trees. No framework component library is registered: every `component` node draws its fallback. Sessions execute on workers; document operations are authorized and committed on the document thread."#;
