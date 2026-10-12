# lark2tapd's native editor patch

This directory contains the **MIT-licensed crates.io release** of
`zorite-editor` **0.11.1**, from the upstream repository
<https://github.com/packetThrower/zorite>, release commit
`3d1b3c0fda23d9f3706d6cac6ddec787c88fd25d`, path `crates/zorite-editor`.
The published `.cargo_vcs_info.json`, original manifest, and unmodified
`LICENSE` are retained. No code from the GPL-licensed Zorite application is
included. `gpui-bidi` 0.1.1 and `zorite-markdown` 0.10.0 are also MIT releases;
the latter is used with default features disabled.

The workspace uses Cargo's standard `[patch.crates-io]` override. The editor
remains excluded from the application workspace so upstream examples and test
configuration do not become application targets. GPUI is shared with
`gpui-kit` 0.7.1 (`gpui-pre` 0.3.8). The only manifest change within this copy
is a test-support dev dependency; its standalone lockfile is updated for those
tests. Application resolution uses the root lockfile.

## Local behavioral changes

- `with_text` / `set_text` keep source bytes unchanged, rather than normalizing
  attached `$$` fences on load. Native Markdown projection is unchanged.
- Native IME marked-text selections are relative to the inserted string, not
  the document. Out-of-range and surrogate-interior UTF-16 offsets clamp to
  legal UTF-8 boundaries. The composition's original content and selection
  are captured once. Commit / unmark create at most one undo step; empty
  composition replacement or undo cancels preedit and restores the original
  selection. Redo first cancels preedit, then applies the next stable history
  entry if available; with an empty redo stack it only cancels composition.
  Undo snapshots retain selection direction.
- `EditorEvent::PasteMedia(ClipboardItem)` takes precedence over accompanying
  text for Image / ExternalPaths entries. Keyboard and context-menu paste use
  the same handler. GPUI delivers the event after the editor borrow ends;
  there is no synchronous host callback.
- **Breaking local event API:** `EditorEvent::Changed` is now
  `EditorEvent::Changed { origin: ChangeOrigin }`. The public `ChangeOrigin`
  enum distinguishes `User` (typing, native IME including cancel, paste, menu
  actions, undo/redo) from `Host` (`set_text`, `replace_range`,
  `resolve_placeholder`, and `append_text`). Both still notify persistence;
  only `User` should trigger host caret autoscroll. Host updates must preserve
  the viewport. Internal structural user actions share an origin-neutral range
  mutation helper and emit one `User` event at the complete action boundary.
  Hosts must not echo either event back through `set_text`.
- `is_composing()` lets the host defer asynchronous edits until native input
  finishes. Composition ending also notifies observers even when only marked
  state changes.
- `resolve_placeholder(marker, replacement, cx) -> bool` returns false without
  modification during composition. Otherwise it replaces every occurrence of
  that unique marker in the current document **and both history stacks**,
  without creating an undo step. Deleted current markers are not reinserted;
  history is still resolved and the call returns true. Empty markers are
  no-ops. Selection endpoints retain offsets around edits and direction;
  endpoints inside a marker map to the replacement's end. Hosts must use a
  unique marker and a final replacement that does not contain that marker.
- `append_text(text, cx)` appends one undoable host edit while preserving the
  current selection and its direction (including a caret at the old end).
  Empty text or active composition is a no-op; the host defers/retries while
  composing.

No disabled-mode API or WebView was introduced.

## Regression checks

From the repository root:

```sh
cargo test --locked
cargo test --locked --manifest-path vendor/zorite-editor/Cargo.toml --lib --target-dir target
cargo test --locked --manifest-path vendor/zorite-editor/Cargo.toml --doc --target-dir target
```

The standalone command is required because Cargo does not run the dev tests of
excluded dependency packages with `cargo test -p`. `src/host_tests.rs` exercises
real GPUI entities and input handlers with `test-support`; upstream projection
tests remain enabled. These tests do not substitute for manual testing with
macOS's actual Chinese IME or actual system clipboard / context menus.

The published demo example imports `os_spellcheck` without declaring that
dependency. An unqualified standalone `cargo test` therefore fails compiling
that unchanged example; `--lib` and `--doc` validate the shipped library.
