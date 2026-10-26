# Native Patch Manager

Build Zed from this checkout and run **patch manager: open** from the Command
Palette. The page opens as a Workspace tab. It uses the current local worktree;
**Source Checkout…** selects another local checkout containing this registry.

Search and filter the registry, open **Details**, and use **Validate** or
**View Diff** before changing source. Diff previews use Zed's existing
`MultiDiffView` and read-only buffers backed by temporary before/after snapshots.
The snapshots live until the diff tab closes.

**Register Patch** opens a native form with a local `.patch` picker, ID, name,
description, category and optional activation. Validation checks metadata, text
patch format, allowed paths and compatibility with the checkout. Registration
copies the patch into this directory and atomically updates `manifest.json`.
Registration and Enable never apply source changes. This registry manages local
patches; it has no remote catalog or upstream update provider.

**Enabled** means eligible to apply. **Applied** means Git detects the complete
patch in the source checkout. Neither means the running editor contains that
change. Apply and Revert require confirmation, saved buffers, and a subsequent
build and restart. A rebuild requirement is retained after Revert as well.
Runtime build provenance is unverified; the manager does not claim runtime
activation or run builds automatically. Conflicts disable source operations.
Removing a registration requires confirmation and retains both source changes
and the patch file, allowing it to be registered again. It also allows cleaning
up registrations whose patch files are missing or invalid.

The page refreshes when opened, refocused, after an operation, and with
**Refresh** / **Validate**. CLI operations use the same service and manifest.
Operations run on GPUI's background executor; Git and filesystem work are not
performed in rendering or UI event handlers. The service serializes operations
across processes with a Git-directory lock and persists manifests atomically.

## CLI

```sh
cargo run -p patch_manager --bin zed-patch -- . list
cargo run -p patch_manager --bin zed-patch -- . validate
cargo run -p patch_manager --bin zed-patch -- . enable wayland-ime-preedit-cursor
cargo run -p patch_manager --bin zed-patch -- . apply wayland-ime-preedit-cursor --confirm-source-change
cargo run -p patch_manager --bin zed-patch -- . revert wayland-ime-preedit-cursor --confirm-source-change
cargo run -p patch_manager --bin zed-patch -- . disable wayland-ime-preedit-cursor
cargo run -p patch_manager --bin zed-patch -- . remove wayland-ime-preedit-cursor --confirm-source-change
cargo run -p patch_manager --bin zed-patch -- . register /path/example.patch /path/metadata.json
```

Registration metadata uses the same `Patch` schema as the manifest. `source`,
`sha256`, `enabled`, and `rebuild_required` are optional; registration generates
source and hash and resets the rebuild marker. For example:

```json
{
  "id": "example",
  "name": "Example patch",
  "description": "Describe the source change",
  "category": "Other"
}
```

Source patches are limited to UTF-8 text, 10 MiB, and safe checkout-relative
paths. Git metadata, the registry, symlinks, submodules and binary patches are
excluded. A stored SHA-256 detects changed patch content. Apply never uses
`--reject`, `--3way`, reset, checkout or stash. External editors and Git commands
do not honor this service's lock; Git rechecks application against current disk
contents and refuses conflicts. Save and review the checkout before applying.

## Core integration

`patch_manager` owns manifest management, validation, state inference, snapshots
and Git operations. Its `zed-patch` binary and the native page share this API.
The page is initialized with `extensions_ui` and reuses Workspace actions/items,
GPUI controls, theme tokens and UI input fields. Registration is a separate UI
module. No extension runtime changes, browser, WebView or server are involved.

The initial `wayland-ime-preedit-cursor` patch fixes Wayland preedit selections:
text-input-v3 byte positions are validated and converted from UTF-8 to GPUI's
UTF-16 positions, then passed to the native input handler. Compose-key handling
continues using its existing marked-text event. The patch includes the actual
source diff and its regression test. The source fix is already present in this
checkout, so the initial registry reports Enabled, Applied and Rebuild Required.

## Verification

```sh
cargo test -p patch_manager
cargo test -p extensions_ui patch_manager_tab_opens_reuses_closes_and_reopens
cargo test -p gpui_linux cursor
cargo check -p extensions_ui --tests
```

Service tests use isolated Git repositories, including a complete validate,
preview, revert and reapply cycle for the built-in patch. The GPUI test covers
action dispatch, tab reuse, closure, reopening and an unavailable-checkout error.
Validation completed: nine service tests, the GPUI tab lifecycle test, four
Wayland cursor tests, CLI operation flows, UI and Wayland compilation checks,
service Clippy checks, and formatting checks passed.

Interactive verification is still needed for platform file pickers, light/dark
appearance, narrow windows, font scaling, confirmation dialogs and live Wayland
IME input.
