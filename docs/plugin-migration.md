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

- TODO(PhotoCraft-plugin-integration): filter menu/category entries, manifest
  parameter dialogs, RGB(A) 8/16/32 conversion and disabled mode explanations.
- TODO(PhotoCraft-plugin-transactions): consume DocumentApi requests on the document
  thread, capture scope/revision, rollback on timeout/revocation, and commit each
  session event as one labelled history entry. Wire command/event workers and helpers.
- TODO(PhotoCraft-plugin-panels): validate and render shared UI Nodes in egui and
  route revision-carrying widget events.
- TODO(PhotoCraft-plugin-resources): implement the advertised plugin.resources query,
  filter presets and LUT integration; discovery currently returns manifest resources.
- TODO(PhotoCraft-plugin-projects): persist plugin references in native document
  metadata, preserve missing non-destructive params and refuse final export with
  the missing plugin's name. Destructive pixel edits need no project record.
- TODO(PhotoCraft-plugin-examples): verify unchanged halftone and film-looks packages
  end to end once document/UI wiring exists. They are not currently usable in menus.

The contract describes the intended operation surface, not an assertion that these
operations are already dispatched by the engine. Shared source is read-only except
for the PhotoCraft contract page.
