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
    /// A `.wav` file — opens it.
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
}

impl FilePanel {
    pub fn new(directory: PathBuf) -> Self {
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
        };
        panel.scan();
        panel
    }

    pub fn scan(&mut self) {
        self.entries = Self::scan_dir(&self.directory);
        let count = self.entries.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        self.clamp_scroll();
    }

    /// Lists `..` (unless at the filesystem root), then subdirectories, then `.wav` files —
    /// dirs and files each sorted case-insensitively.
    pub fn scan_dir(dir: &Path) -> Vec<FileEntry> {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        if let Ok(readdir) = std::fs::read_dir(dir) {
            for entry in readdir.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                    continue;
                };
                if path.is_dir() {
                    dirs.push(FileEntry { name, path, kind: EntryKind::Dir });
                } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
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
        self.filtered_entries().len()
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
        let max_visible = 30usize;
        if self.selected >= self.scroll_offset + max_visible {
            self.scroll_offset = self.selected.saturating_sub(max_visible.saturating_sub(1));
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

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        self.rects.clear();

        let title = format!(" Files ({}) ", self.entries.len());

        let border_style = if self.focused {
            Style::default().fg(theme::FOCUS)
        } else {
            Style::default().fg(theme::BORDER)
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style)
            .style(Style::default().bg(theme::BASE));
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
                Style::default().fg(theme::TEXT).bg(theme::SURFACE0)
            } else if is_folder {
                Style::default().fg(theme::SKY).bg(theme::BASE)
            } else if is_dirty {
                Style::default().fg(theme::DIRTY).bg(theme::BASE)
            } else {
                Style::default().fg(theme::TEXT).bg(theme::BASE)
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
        let entries = FilePanel::scan_dir(dir);
        assert!(entries.len() >= 2, "expected at least 2 .wav files in tests/fixtures, found {}", entries.len());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"mono_sine.wav"));
        assert!(names.contains(&"stereo_sine.wav"));
    }

    #[test]
    fn scan_lists_parent_then_dirs_then_files() {
        use std::fs;
        let base = std::env::temp_dir().join("tui_wave_dirtest");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("subdir")).unwrap();
        fs::write(base.join("a.wav"), b"x").unwrap(); // scan only checks the extension
        let entries = FilePanel::scan_dir(&base);

        assert_eq!(entries[0].name, "..");
        assert!(matches!(entries[0].kind, EntryKind::Parent));
        let dir_pos = entries.iter().position(|e| e.name == "subdir").unwrap();
        let file_pos = entries.iter().position(|e| e.name == "a.wav").unwrap();
        assert!(matches!(entries[dir_pos].kind, EntryKind::Dir));
        assert!(matches!(entries[file_pos].kind, EntryKind::File));
        assert!(dir_pos < file_pos, "directories should sort before files");

        fs::remove_dir_all(&base).unwrap();
    }
}
