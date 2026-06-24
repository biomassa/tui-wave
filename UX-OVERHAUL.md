# UX/UI Overhaul — modal command panel

Major paradigm change: the command panel (toolbar) becomes **modal** — its contents depend
on which panel is focused. This file is the persistent step-by-step plan; check items off as
they land. Keep it until the whole overhaul is implemented and verified, then delete.

Focus states: **Waveform**, **Files**, **Buffers**.

---

## 0. Foundations — explicit focus model
- [x] `enum Focus { Waveform, Files, Buffers }` + `App::focus()` derived from the panel
      `.focused` flags. Drives modal toolbar, contextual keys, waveform header color.

## 1. Modal command panel (toolbar)
- [x] `Toolbar` holds three command sets keyed by `Focus`; `App::render` passes the current
      focus to `Toolbar::render`/`rows_needed`, which render the matching set.
- [x] Command sets:
  - **Waveform** (current): Play · FILE · EDIT · VIEW · PROCESS · MARK · OPTS.
  - **Files**: Open (Enter), Open Directory (^o), Select File (Up/Dn), Search (/), Focus (Tab), Quit (q).
  - **Buffers**: Save (^s), Close (^w), Rename (^r), Save All (^a).
- [x] Adaptive height uses the active set's width.
- [x] Mouse clicks on modal buttons dispatch their action (handled in `handle_action`).

## 2. New actions + contextual key handling
- [x] Added actions: `Noop`, `OpenSelected`, `OpenDirectory`, `SearchFiles`, `FocusNext`,
      `CloseBuffer`, `RenameBuffer` (reuse `Save`/`SaveAll`).
- [x] Files-focus key: `^o` → Open Directory dialog (in the file-panel branch).
- [x] Buffers-focus keys: `^s` Save, `^w` Close, `^r` Rename, `^a` Save All — resolved in the
      buffer-panel branch before the global keymap, so `^r`/`^a` don't collide with Reverse/SaveAll.

## 3. Files panel — directory awareness
- [x] `FileEntry` gains a kind: Parent (`..`) | Dir | File (`file_panel::EntryKind`).
- [x] `scan_dir` lists `..` (unless at root), then sub-directories, then `.wav` files —
      dirs-before-files, each case-insensitively sorted.
- [x] Enter / click on a directory (or `..`) navigates (`set_directory`); on a file, opens it.
- [x] Render: folders in `theme::SKY` with a trailing `/`; `..` first.
- [x] Filter (`/`) still filters the current directory's entries; Up/Dn navigate the filtered
      list.

## 4. Open Directory dialog (^o)
- [x] Dialog typing a directory path, default `~` (expanded via $HOME). On Enter: if it's an
      existing dir, point the file panel at it + rescan (`FilePanel::set_directory`); else no-op.

## 5. Buffers panel — Close / Rename / Save All
- [x] **Close (^w)**: `request_close_buffer` confirms if dirty (generalized `Confirm` modal),
      then `close_buffer` removes the doc + its parallel history, fixes `active_document`, and
      rebuilds audio/caches/viewport. Closing the last buffer → empty state.
- [x] **Rename (^r)**: dialog → `rename_buffer` renames the file on disk (same dir) and updates
      `document.path` + file-panel dirty map; for an unsaved buffer it just sets the path.
- [x] **Save All (^a)** / **Save (^s)**: reuse `save_all()` / Save.

## 6. Waveform header color (quick)
- [x] The title text `tui-wave — <name>` uses `theme::FOCUS` (orange) when the waveform is
      focused, matching its border; `theme::BORDER` otherwise.

## 7. Bugfix: Copy to New must mark the buffer dirty (quick)
- [x] In `Action::CopyToNew`, set `new_doc.dirty = true` so the unsaved-changes quit confirmation
      triggers (currently it's created clean, so `q` exits without warning). Test added.

## 8. Docs + tests
- [x] Updated `CLAUDE.md`: modal command panel, focus model, dir-aware file panel, contextual keys.
- [x] Tests: close-buffer index/history math; directory scan ordering (parent/dir/file);
      copy-to-new marks dirty.

---

**Status: implemented (2026-06-24), 92 tests pass, zero warnings.** Pending the user's visual
verification of the modal panel + file browser, then delete this file.

---

### Suggested order
Quick isolated wins first (6, 7), then foundations (0) → modal panel (1) → actions/keys (2) →
file-panel dir-awareness (3) + open-dir dialog (4) → buffers close/rename (5) → docs/tests (8).
