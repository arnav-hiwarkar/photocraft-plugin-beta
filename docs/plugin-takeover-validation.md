# PhotoCraft plugin recovery validation

Recovery base: `b367fce82` (PhotoCraft). The existing dirty session/UI/CLI work was retained and completed in place, as requested. This change uses local sibling `craft-extensions` sources. No remote publishing, CI changes, or release builds of the app were performed.

## Behavior and failure model

PhotoCraft now provides `photocraft.session@1.1`: shared host-rendered UI, plugin tools and overlays, pixels/layers/selection/guides/histograms/adjustments, and plugin-owned picked-file handles. Desktop and CLI use the same engine operations and live permission checks. A gesture folds edits into one undo step; cancellation restores its document snapshot.

The recovery repairs address these confirmed gaps:

- Validate save-handle names as well as folder-child names; executable and hidden names are refused.
- Publish staged files using `photocraft_format::atomic_write_no_follow`, so a final-component symlink planted after staging cannot redirect a write.
- Acknowledge event finish only after file publication and document/UI finalization. A failed publication returns an error and restores document/history/revision/journal; staged success UI and picks are discarded.
- Files are published in order with **per-file** atomic replacement. Stop at the first failure. Files already published remain, and the error reports their count. The failing destination may also have changed if replacement succeeded but its final durability flush failed. This is explicitly **not multi-file atomicity**, and those files are not undone by document Undo. The host does not promise to roll files back after a crash or an external filesystem change.
- Refuse `files.*`, `document.export` and `layer.import` during a tool gesture, including events from other contributions while that gesture is active.
- Composite reads use the rectangle compositor. Histograms use 256×256 tiles and refuse areas above 64 Mi pixels before reading; larger images may supply `rect`. This bounds materialized pixel buffers and total work, but is not a measured wall-clock guarantee.
- The layer-export example cancels if the document changed while the folder dialog was open, and keeps Unicode filenames below the host byte limit. Gradient painting tiles in both dimensions to respect the call argument limit on wide canvases.

Filesystem confinement assumes the selected parent directory is not concurrently replaced or moved by another local process. Final-component symlink substitution is tested; this is not an OS directory-handle capability sandbox against a hostile local filesystem actor.

## Checks

The resource-heavy engine test compilation was interrupted by a tool-host outage; no passing result was observed. The earlier example integration compiled but was ignored without fixtures. No new source check is claimed for the final recovery edits.

The user explicitly stopped builds and broad test runs. Continue with source review and, only when genuinely cheap, one scoped `cargo check -p <crate>` with `CARGO_BUILD_JOBS=1`. Do not run example release builds, app builds, `cargo run`, or broad tests. Defer those checks to a later user-authorized validation phase.

Residual review work: revalidate cancellation and live grants after synchronous file publication before accepting document commit. Final-component no-follow writes do not prevent concurrent parent-directory replacement. The generated shared PhotoCraft contract page still needs synchronization from the source; do not run the CLI to generate it during this source-only phase.

## Regression coverage

New engine tests cover save-name validation, a symlink planted after staging, failure after one file has published (later writes stop), document/history rollback on publication failure, gesture file-operation refusal, histogram tile boundaries/work limits, and a one-pixel composite read from a giant sparse canvas. Existing session tests cover authorization-related op restrictions, layer locks, undo folding, cancellation and malformed params.

The ignored integration test requires actual release-built guest packages and a disposable `CRAFT_EXT_DIR`. It exercises Gradient Ramp gesture commit/undo/Escape/revocation, Levels Scope panel/command, Layer Export and failed publication, and pause/resume. Merely compiling that test or observing its default ignored result is not counted as a pass.

UI screenshots, wasm app gates, performance benchmarks, full corpus runs, Windows/macOS execution and remote CI must be reported separately; unit or headless integration results do not prove those behaviors.
