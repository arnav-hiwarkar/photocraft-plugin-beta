# Plugins

PhotoCraft is migrating to the shared Craft schema-2 plugin platform in
`craft-extensions`. The former `pc_*` ABI and loose Wasm folder loading are removed.
Use `photocraft-cli plugin --help` for management and `plugin contract` for the
adapter contract. `PHOTOCRAFT_SAFE_MODE=1` skips plugin execution, while management
remains available.

Implementation status: contract, discovery, shared transform boundary, CLI management,
plugin LUTs/presets and effect filters (Filter › Plugins, `plugin.<plugin-id>/<contribution>`
commands with generated dialogs; see [plugin-migration.md](plugin-migration.md)) are
implemented. Document transactions, panels and project persistence remain to be wired.
