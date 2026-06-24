# UX/UI Overhaul — modal command panel

Major paradigm change: the command panel (toolbar) becomes **modal** — its contents depend
on which panel is focused. This file is the persistent step-by-step plan; check items off as
they land. Keep it until the whole overhaul is implemented and verified, then delete.

Focus states: **Waveform**, **Files**, **Buffers**.

---

## 0. Foundations — explicit focus model
- [ ] Introduce a single source of truth for focus, e.g. `enum Focus { Waveform, Files, Buffers }`
      derived from the existing `file_panel.focused` / `buffer_panel.focused` flags (waveform =
      neither focused). Use it for: modal toolbar, contextual keys, waveform header color.

## 1. Modal command panel (toolbar)
- [ ] `Toolbar` holds three command sets (groups) keyed by `Focus`; `App` passes the current
      focus to `Toolbar::render`, which renders the matching set.
- [ ] Command sets:
  - **Waveform** (current): Play · FILE · EDIT · VIEW · PROCESS · MARK · OPTS.
  - **Files**: Open (Enter), Open Directory (^o), Select File (Up/Dn), Search (/), Focus (Tab), Quit (q).
  - **Buffers**: Save (^s), Close (^w), Rename (^r), Save All (^a).
- [ ] `rows_needed`/adaptive height use the active set's width.
- [ ] Mouse clicks on modal buttons dispatch the right action for that focus.

## 2. New actions + contextual key handling
- [ ] Add actions: `OpenDirectory`, `CloseBuffer`, `RenameBuffer` (reuse `Save`/`SaveAll`).
- [ ] Files-focus keys (handled in the file-panel branch of `handle_key`): `^o` → Open Directory
      dialog. (Enter / Up / Dn / `/` / Tab already handled.)
- [ ] Buffers-focus keys (handled in the buffer-panel branch): `^s` Save active, `^w` Close,
      `^r` Rename, `^a` Save All. NOTE: `^r`/`^a` mean Rename/SaveAll *only while the Buffers
      panel is focused* — they must not collide with the global Reverse(^r)/SaveAll(^l). Resolve
      contextually in the focus branch, before falling through to the global keymap.

## 3. Files panel — directory awareness
- [ ] `FileEntry` gains a kind: Parent (`..`) | Dir | File.
- [ ] `scan_dir` lists: `..` (unless at filesystem root), sub-directories, then `.wav` files.
      Sort dirs-before-files, each alphabetically.
- [ ] Enter on a directory (or `..`) navigates: set `file_panel.directory`, rescan, reset
      selection/scroll. Enter on a file opens it (existing path).
- [ ] Render: visually distinguish folders (trailing `/` and/or distinct color); `..` first.
- [ ] Filter (`/`) still filters the current directory's entries (incl. dirs) and Up/Dn navigate
      the filtered list (verify existing behavior survives the dir changes).

## 4. Open Directory dialog (^o)
- [ ] New dialog: type a directory path, default text `~`. Expand `~` to the home dir on submit.
- [ ] On Enter: if the path is an existing directory, point the file panel at it + rescan; else
      ignore (or show a brief error). Esc cancels.

## 5. Buffers panel — Close / Rename / Save All
- [ ] **Close (^w)**: remove the active document AND its parallel history entry; fix
      `active_document`; rebuild audio/caches/viewport. If the buffer is dirty, confirm first
      (reuse/adapt the quit-confirm pattern). Closing the last buffer → empty state.
- [ ] **Rename (^r)**: dialog for a new name. If the doc has a path, rename the file on disk and
      update `document.path` + `file_panel` dirty map; if no path, just set the path/name.
- [ ] **Save All (^a)** and **Save (^s)**: reuse existing `save_all()` / Save.

## 6. Waveform header color (quick)
- [ ] The title text `tui-wave — <name>` uses `theme::FOCUS` (orange) when the waveform is
      focused, matching its border; `theme::BORDER` otherwise.

## 7. Bugfix: Copy to New must mark the buffer dirty (quick)
- [ ] In `Action::CopyToNew`, set `new_doc.dirty = true` so the unsaved-changes quit confirmation
      triggers (currently it's created clean, so `q` exits without warning).

## 8. Docs + tests
- [ ] Update `CLAUDE.md`: modal command panel, focus model, dir-aware file panel, contextual keys.
- [ ] Tests: focus→command-set mapping; close-buffer index/history math; directory navigation
      (enter dir, `..`); copy-to-new marks dirty.

---

### Suggested order
Quick isolated wins first (6, 7), then foundations (0) → modal panel (1) → actions/keys (2) →
file-panel dir-awareness (3) + open-dir dialog (4) → buffers close/rename (5) → docs/tests (8).
