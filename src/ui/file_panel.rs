use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::theme;

/// What a file-panel row represents.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// The `..` row — navigates to the parent directory.
    Parent,
    /// A subdirectory — navigates into it.
    Dir,
    /// A file matching the panel's own extension filter (see `FilePanel::extension`) —
    /// opens/selects it.
    File,
}

#[derive(Clone)]
pub(crate) struct FileEntry {
    name: String,
    path: PathBuf,
    kind: EntryKind,
}

pub struct FilePanel {
    pub directory: PathBuf,
    entries: Vec<FileEntry>,
    pub selected: usize,
    scroll_offset: usize,
    pub filter: String,
    pub filtering: bool,
    pub focused: bool,
    pub dirty_paths: HashSet<PathBuf>,
    rects: Vec<Rect>,
    /// Number of entry rows actually visible at last render, used both to clamp scrolling
    /// and as the page size for `move_page_up`/`move_page_down`. Updated every render, so
    /// PgUp/PgDn always moves by exactly one screenful regardless of terminal size.
    visible_rows: usize,
    /// The file extensions this panel lists (case-insensitive, no leading dot) —
    /// `io::IMPORT_EXTENSIONS` for the main Files panel, `["pc"]` for the "Load Pitch
    /// Curve..." picker (`Dialog::LoadCurve`), CDP's own pitch-curve save format. A slice
    /// rather than one extension because the main panel now lists every importable audio
    /// format intermixed, sorted by name like any other listing.
    extensions: &'static [&'static str],
    /// Title shown in the panel's own border (`" {label} (N) "`) — "Files" for the main
    /// panel, something more specific (e.g. "Load Pitch Curve") for a picker reusing this
    /// widget inside a modal dialog.
    pub label: &'static str,
}

impl FilePanel {
    pub fn new(directory: PathBuf) -> Self {
        Self::new_with(directory, crate::model::io::IMPORT_EXTENSIONS, "Files")
    }

    /// A picker variant filtered to different extensions than the main panel's importable
    /// audio formats — see `extensions`'s doc comment.
    pub fn new_with_extension(
        directory: PathBuf,
        extensions: &'static [&'static str],
        label: &'static str,
    ) -> Self {
        Self::new_with(directory, extensions, label)
    }

    fn new_with(directory: PathBuf, extensions: &'static [&'static str], label: &'static str) -> Self {
        let mut panel = Self {
            directory,
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            filter: String::new(),
            filtering: false,
            focused: false,
            dirty_paths: HashSet::new(),
            rects: Vec::new(),
            visible_rows: 10,
            extensions,
            label,
        };
        panel.scan();
        panel
    }

    pub fn scan(&mut self) {
        self.entries = Self::scan_dir(&self.directory, self.extensions);
        let count = self.entries.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        self.clamp_scroll();
    }

    /// Lists `..` (unless at the filesystem root), then subdirectories, then files whose
    /// extension matches any of `extensions` (case-insensitive) — dirs and files each sorted
    /// case-insensitively, so the accepted formats intermix in one alphabetical list.
    pub fn scan_dir(dir: &Path, extensions: &[&str]) -> Vec<FileEntry> {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(readdir) = std::fs::read_dir(dir) {
            for entry in readdir.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                // Hidden entries are skipped — **directories as well as files**. A home
                // directory is mostly `.config`, `.cache`, `.local` and friends, and burying
                // the two folders someone actually keeps audio in among thirty dotfiles makes
                // the panel harder to read for no gain: nothing this app opens is ever stored
                // in one. The `..` row is unaffected, being synthesised below rather than read
                // from the listing.
                if name.starts_with('.') {
                    continue;
                }
                if path.is_dir() {
                    dirs.push(FileEntry { name, path, kind: EntryKind::Dir });
                } else if path
                    .extension()
                    .is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)))
                {
                    files.push(FileEntry { name, path, kind: EntryKind::File });
                }
            }
        }
        let by_name = |a: &FileEntry, b: &FileEntry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
        dirs.sort_by(by_name);
        files.sort_by(by_name);

        let mut entries = Vec::new();
        if let Some(parent) = dir.parent() {
            entries.push(FileEntry {
                name: "..".to_string(),
                path: parent.to_path_buf(),
                kind: EntryKind::Parent,
            });
        }
        entries.extend(dirs);
        entries.extend(files);
        entries
    }

    /// Repoints the panel at a new directory and rescans, resetting selection/scroll/filter.
    pub fn set_directory(&mut self, path: PathBuf) {
        self.directory = path;
        self.selected = 0;
        self.scroll_offset = 0;
        self.filter.clear();
        self.filtering = false;
        self.scan();
    }

    pub fn mark_dirty(&mut self, path: &Path, dirty: bool) {
        if dirty {
            self.dirty_paths.insert(path.to_path_buf());
        } else {
            self.dirty_paths.remove(path);
        }
    }

    /// The currently-selected entry's path and kind, so the caller can decide whether to
    /// navigate into a directory or open a file.
    pub fn selected_entry(&self) -> Option<(PathBuf, EntryKind)> {
        self.nth_filtered_entry(self.selected).map(|e| (e.path.clone(), e.kind))
    }

    fn nth_filtered_entry(&self, n: usize) -> Option<&FileEntry> {
        let filter = &self.filter;
        let lower = filter.to_lowercase();
        self.entries.iter().filter(|e| {
            filter.is_empty() || e.name.to_lowercase().contains(&lower)
        }).nth(n)
    }

    pub fn filtered_count(&self) -> usize {
        // Count through the iterator rather than cloning the whole filtered list just to
        // read its length — this runs every render via `clamp_scroll`.
        let lower = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| self.filter.is_empty() || e.name.to_lowercase().contains(&lower))
            .count()
    }

    fn filtered_entries(&self) -> Vec<FileEntry> {
        let filter = &self.filter;
        let lower = filter.to_lowercase();
        self.entries.iter().filter(move |e| {
            filter.is_empty() || e.name.to_lowercase().contains(&lower)
        }).cloned().collect()
    }

    fn clamp_scroll(&mut self) {
        let count = self.filtered_count();
        if self.selected >= count && count > 0 {
            self.selected = count - 1;
        } else if count == 0 {
            self.selected = 0;
        }
        // Keep selected in view
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + self.visible_rows {
            self.scroll_offset = self.selected.saturating_sub(self.visible_rows.saturating_sub(1));
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.clamp_scroll();
        }
    }

    pub fn move_down(&mut self) {
        let count = self.filtered_count();
        if self.selected + 1 < count {
            self.selected += 1;
            self.clamp_scroll();
        }
    }

    pub fn move_top(&mut self) {
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn move_bottom(&mut self) {
        let count = self.filtered_count();
        if count > 0 {
            self.selected = count - 1;
            self.clamp_scroll();
        }
    }

    /// Moves the selection up by one screenful — for browsing directories with many files
    /// without holding Up.
    pub fn move_page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.visible_rows.max(1));
        self.clamp_scroll();
    }

    /// Moves the selection down by one screenful.
    pub fn move_page_down(&mut self) {
        let count = self.filtered_count();
        if count > 0 {
            self.selected = (self.selected + self.visible_rows.max(1)).min(count - 1);
            self.clamp_scroll();
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.render_with(frame, area, Borders::ALL, theme::BASE);
    }

    /// Render as a **column inside a dialog** rather than as a standalone panel: one vertical
    /// divider on the right instead of a full box, and the dialog's own surface colour instead
    /// of the editor background.
    ///
    /// The same treatment `Dialog::CdpBrowser`'s Domain/Groups columns get, and for the same
    /// reason — a fully-boxed list nested inside a dialog's own border reads as a cramped
    /// panel-within-a-panel (user report, 2026-08-07), and the two backgrounds meeting at that
    /// inner border make the contrast worse. A single rule separating two columns is enough to
    /// say "these are different things".
    ///
    /// `Borders::RIGHT` rather than `LEFT` (which is what `CdpBrowser` uses) because this
    /// column is the leftmost thing in its dialog: the divider belongs between the list and the
    /// form, not against the popup's own edge.
    pub fn render_column(&mut self, frame: &mut Frame, area: Rect) {
        // No border at all: the hosting column draws one full-height divider of its own, so the
        // list rows can run the whole width and the rule is unbroken by the header and footer
        // rows above and below them. A per-list `Borders::RIGHT` produced a divider that started
        // and stopped partway down the column, which reads as a rendering fault.
        self.render_with(frame, area, Borders::NONE, theme::SURFACE0);
    }

    fn render_with(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        borders: Borders,
        background: ratatui::style::Color,
    ) {
        self.rects.clear();

        let title = format!(" {} ({}) ", self.label, self.entries.len());

        let border_style = if self.focused {
            Style::default().fg(theme::FOCUS)
        } else {
            Style::default().fg(theme::BORDER)
        };
        let mut block = Block::default()
            .borders(borders)
            .border_style(border_style)
            .style(Style::default().bg(background));
        // A full box carries its own title; a column has no top edge to hang one on, so the
        // hosting dialog labels it instead.
        if borders == Borders::ALL {
            block = block.title(title);
        }
        let inner = block.inner(area);

        frame.render_widget(block, area);

        if inner.width < 3 || inner.height < 1 {
            return;
        }

        let filter_line = if self.filtering {
            1
        } else {
            0
        };
        let mut y = inner.y;
        let x = inner.x;

        // Draw filter line if filtering
        if self.filtering {
            let filter_text = format!("/{}_", self.filter);
            let style = Style::default().fg(theme::PEACH).bg(theme::SURFACE0);
            frame.render_widget(Paragraph::new(filter_text).style(style), Rect {
                x,
                y,
                width: inner.width,
                height: 1,
            });
            y += 1;
        }

        let inner_height = inner.height.saturating_sub(filter_line) as usize;
        self.visible_rows = inner_height.max(1);
        self.clamp_scroll();

        let filtered = self.filtered_entries();
        for (idx, entry) in filtered.iter().enumerate().skip(self.scroll_offset).take(inner_height) {
            let is_selected = idx == self.selected;
            let is_folder = matches!(entry.kind, EntryKind::Parent | EntryKind::Dir);
            let is_dirty = !is_folder && self.dirty_paths.contains(&entry.path);

            // Folders get a trailing "/" (except ".."); dirty files get a "*" prefix.
            let mut display = match entry.kind {
                EntryKind::Dir => format!("{}/", entry.name),
                _ => entry.name.clone(),
            };
            if is_dirty {
                display = format!("*{}", display);
            }

            let display_len = display.chars().count();
            let truncated: String = if display_len > inner.width as usize {
                if display_len > 3 {
                    let tail: String = display
                        .chars()
                        .skip(display_len.saturating_sub(inner.width as usize - 1))
                        .collect();
                    format!("…{}", tail)
                } else {
                    display.chars().take(inner.width as usize).collect()
                }
            } else {
                display
            };

            let style = if is_selected && self.focused {
                Style::default().fg(theme::HIGHLIGHT_FG).bg(theme::HIGHLIGHT_BG)
            } else if is_selected {
                Style::default().fg(theme::TEXT).bg(theme::SURFACE1)
            } else if is_folder {
                Style::default().fg(theme::SKY).bg(background)
            } else if is_dirty {
                Style::default().fg(theme::DIRTY).bg(background)
            } else {
                Style::default().fg(theme::TEXT).bg(background)
            };

            self.rects.push(Rect {
                x,
                y,
                width: inner.width,
                height: 1,
            });

            frame.render_widget(Paragraph::new(Line::from(Span::styled(truncated, style))), Rect {
                x,
                y,
                width: inner.width,
                height: 1,
            });
            y += 1;
        }
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<usize> {
        self.rects
            .iter()
            .position(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    }

    /// Handle a mouse click: select the clicked entry. Returns `true` if a row was hit, so
    /// the caller can activate it (navigate into a dir / open a file).
    pub fn handle_click(&mut self, x: u16, y: u16) -> bool {
        let Some(rect_idx) = self.hit_test(x, y) else {
            return false;
        };
        let entry_idx = self.scroll_offset + rect_idx;
        if entry_idx < self.filtered_count() {
            self.selected = entry_idx;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_finds_wav_files() {
        let dir = Path::new("tests/fixtures");
        let entries = FilePanel::scan_dir(dir, &["wav"]);
        assert!(entries.len() >= 2, "expected at least 2 .wav files in tests/fixtures, found {}", entries.len());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"mono_sine.wav"));
        assert!(names.contains(&"stereo_sine.wav"));
    }

    /// Hidden entries are skipped, and **directories count as entries**: a home directory is
    /// mostly `.config`/`.cache`/`.local`, and listing them buries the folders someone actually
    /// keeps audio in. The `..` row must survive, since it is synthesised rather than read from
    /// the listing — the one name starting with a dot that has to stay.
    #[test]
    fn scan_hides_dotfiles_and_dot_directories_but_keeps_the_parent_row() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("tui_wave_hidden_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("visible.wav"), b"x").unwrap();
        fs::write(base.join(".hidden.wav"), b"x").unwrap();
        fs::create_dir_all(base.join("sounds")).unwrap();
        fs::create_dir_all(base.join(".config")).unwrap();

        let entries = FilePanel::scan_dir(&base, &["wav"]);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["..", "sounds", "visible.wav"]);
        assert!(matches!(entries[0].kind, EntryKind::Parent), "the parent row must come first");

        fs::remove_dir_all(&base).unwrap();
    }

    /// The main panel lists every importable audio format intermixed in one alphabetical
    /// list — and the `.mp3` fixture proves export-only formats stay out of it.
    #[test]
    fn scan_lists_every_importable_format_and_excludes_the_rest() {
        use std::fs;
        let base = std::env::temp_dir().join(format!("tui_wave_multiformat_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        // `scan_dir` only looks at extensions, so the contents don't matter here.
        for name in ["b.wav", "a.flac", "d.AIFF", "c.aif", "e.mp3", "f.txt"] {
            fs::write(base.join(name), b"x").unwrap();
        }

        let entries = FilePanel::scan_dir(&base, crate::model::io::IMPORT_EXTENSIONS);
        let names: Vec<&str> = entries
            .iter()
            .filter(|e| matches!(e.kind, EntryKind::File))
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["a.flac", "b.wav", "c.aif", "d.AIFF"],
            "importable formats intermix and sort by name; .mp3 and .txt are excluded",
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scan_lists_parent_then_dirs_then_files() {
        use std::fs;
        let base = std::env::temp_dir().join("tui_wave_dirtest");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("subdir")).unwrap();
        fs::write(base.join("a.wav"), b"x").unwrap(); // scan only checks the extension
        let entries = FilePanel::scan_dir(&base, &["wav"]);

        assert_eq!(entries[0].name, "..");
        assert!(matches!(entries[0].kind, EntryKind::Parent));
        let dir_pos = entries.iter().position(|e| e.name == "subdir").unwrap();
        let file_pos = entries.iter().position(|e| e.name == "a.wav").unwrap();
        assert!(matches!(entries[dir_pos].kind, EntryKind::Dir));
        assert!(matches!(entries[file_pos].kind, EntryKind::File));
        assert!(dir_pos < file_pos, "directories should sort before files");

        fs::remove_dir_all(&base).unwrap();
    }

    /// PgDn/PgUp must move by a full screenful (`visible_rows`, as set by the last render)
    /// rather than a single row, and must clamp at the list's ends like Home/End do.
    #[test]
    fn page_up_and_down_move_by_a_screenful() {
        use std::fs;
        let base = std::env::temp_dir().join("tui_wave_pagetest");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        for i in 0..50 {
            fs::write(base.join(format!("track_{i:03}.wav")), b"x").unwrap();
        }

        let mut panel = FilePanel::new(base.clone());
        panel.visible_rows = 10;
        assert_eq!(panel.selected, 0);

        panel.move_page_down();
        assert_eq!(panel.selected, 10);
        panel.move_page_down();
        assert_eq!(panel.selected, 20);

        panel.move_page_up();
        assert_eq!(panel.selected, 10);

        // 51 entries total (50 files + "..") — paging down past the end clamps at the last.
        panel.selected = 45;
        panel.move_page_down();
        assert_eq!(panel.selected, 50);
        panel.move_page_down();
        assert_eq!(panel.selected, 50, "paging past the end should clamp, not panic or wrap");

        // Paging up past the start clamps at 0.
        panel.selected = 5;
        panel.move_page_up();
        assert_eq!(panel.selected, 0);
        panel.move_page_up();
        assert_eq!(panel.selected, 0, "paging past the start should clamp at 0");

        fs::remove_dir_all(&base).unwrap();
    }
}
