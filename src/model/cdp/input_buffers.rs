//! Persisting a variadic-input CDP process's picked *buffers* — the "input buffers" row in
//! `Dialog::CdpParams` — so a saved preset (and the "Recall last process" auto-save) restores
//! the extra inputs it was built with, not just the numeric params.
//!
//! A pick is a set of open documents, which no `ParamValue` can express: `CdpPreset.values` is
//! index-parallel to `ProcessDef.params`, and the extra-input row is dialog chrome rather than
//! a param. So the picks ride alongside `values` in their own field instead
//! (`CdpPreset.input_buffers`, `LastProcess.input_buffers`), both `#[serde(default)]` so every
//! preset file written before this existed still loads unchanged.
//!
//! **Identity is by path first, then display name.** A saved reference keeps both: `path` is
//! unambiguous but only exists for a buffer that has been saved to disk, while `name` is what
//! the user actually saw in the picker and is the only handle a never-saved scratch buffer
//! has. Matching path-first means two same-named buffers from different directories resolve
//! correctly, and a never-saved buffer still resolves at all.
//!
//! **Resolution is all-or-nothing** (`resolve_against`): if even one saved reference no longer
//! matches an open buffer, the whole pick resets to the dialog's default "1 file: selection
//! only" state rather than partially restoring. A half-restored pick is worse than none —
//! order is meaningful to these processes (`crystal rotate`'s Nth file drives the Nth vertex,
//! `repair`'s groups are positional), so silently dropping one entry shifts every later one
//! into a different role than the preset recorded.

use serde::{Deserialize, Serialize};

/// One saved reference to a buffer that was picked into a variadic process's input list.
/// See the module docs for why both fields are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdpBufferRef {
    /// The buffer's file path, if it has ever been saved. `None` for a scratch buffer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The buffer's display name as shown in the Buffers panel and the picker.
    pub name: String,
}

/// A whole variadic pick, ready to persist: outer `Vec` is index-parallel to
/// `CdpVariadicInput.groups` (one entry per input group — one for a flat `VariadicWav`, two
/// for `GroupedWav`'s channel roles), inner `Vec` is that group's picks **in pick order**,
/// which is the order they reach CDP's command line.
pub type CdpInputBuffers = Vec<Vec<CdpBufferRef>>;

/// One open buffer, as `resolve_against` sees it — the caller supplies these from its own
/// document list so this module needs no knowledge of `Document` or `App`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenBuffer<'a> {
    /// Index into the caller's own document list; handed straight back in the result.
    pub index: usize,
    pub path: Option<&'a str>,
    pub name: &'a str,
}

/// Resolves a saved pick against the currently-open buffers.
///
/// Returns the same group/pick-order shape as `saved`, with each `CdpBufferRef` replaced by
/// the `index` of the open buffer it matched. Returns `None` — meaning "reset the picker to
/// its default state" — if `saved`'s group count doesn't match `expected_groups` (the process
/// def changed shape since the preset was written), or if **any** single reference fails to
/// resolve. See the module docs for why that is deliberately all-or-nothing.
///
/// An empty `saved` with the right group count resolves to empty groups, not `None`: a preset
/// saved with nothing picked is a legitimate pick meaning "selection only", and restoring it
/// as such is exactly right.
pub fn resolve_against(
    saved: &[Vec<CdpBufferRef>],
    open: &[OpenBuffer<'_>],
    expected_groups: usize,
) -> Option<Vec<Vec<usize>>> {
    if saved.len() != expected_groups {
        return None;
    }
    let mut resolved = Vec::with_capacity(saved.len());
    for group in saved {
        let mut indices = Vec::with_capacity(group.len());
        for reference in group {
            indices.push(resolve_one(reference, open)?);
        }
        resolved.push(indices);
    }
    Some(resolved)
}

/// Path match first, display-name match second. Within each pass the first open buffer to
/// match wins; with two buffers sharing a name and neither saved to disk there is genuinely
/// nothing left to disambiguate on, and picking the first is at least stable.
fn resolve_one(reference: &CdpBufferRef, open: &[OpenBuffer<'_>]) -> Option<usize> {
    if let Some(saved_path) = reference.path.as_deref() {
        if let Some(b) = open.iter().find(|b| b.path == Some(saved_path)) {
            return Some(b.index);
        }
    }
    open.iter().find(|b| b.name == reference.name).map(|b| b.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(path: Option<&str>, name: &str) -> CdpBufferRef {
        CdpBufferRef { path: path.map(str::to_string), name: name.into() }
    }

    fn open<'a>(entries: &'a [(usize, Option<&'a str>, &'a str)]) -> Vec<OpenBuffer<'a>> {
        entries.iter().map(|&(index, path, name)| OpenBuffer { index, path, name }).collect()
    }

    #[test]
    fn every_reference_resolving_yields_the_same_shape_with_indices() {
        let buffers = open(&[
            (0, Some("/a/one.wav"), "one.wav"),
            (1, Some("/a/two.wav"), "two.wav"),
            (2, Some("/a/three.wav"), "three.wav"),
        ]);
        let saved = vec![
            vec![r(Some("/a/three.wav"), "three.wav")],
            vec![r(Some("/a/two.wav"), "two.wav"), r(Some("/a/one.wav"), "one.wav")],
        ];
        assert_eq!(resolve_against(&saved, &buffers, 2), Some(vec![vec![2], vec![1, 0]]));
    }

    /// Order is the whole ordering mechanism for these processes, so it must survive
    /// resolution verbatim rather than being re-sorted into document order.
    #[test]
    fn pick_order_is_preserved_not_sorted() {
        let buffers = open(&[(0, Some("/a.wav"), "a.wav"), (1, Some("/b.wav"), "b.wav")]);
        let saved = vec![vec![r(Some("/b.wav"), "b.wav"), r(Some("/a.wav"), "a.wav")]];
        assert_eq!(resolve_against(&saved, &buffers, 1), Some(vec![vec![1, 0]]));
    }

    #[test]
    fn one_missing_buffer_resets_the_whole_pick() {
        let buffers = open(&[(0, Some("/a/one.wav"), "one.wav")]);
        let saved = vec![vec![
            r(Some("/a/one.wav"), "one.wav"),
            r(Some("/a/gone.wav"), "gone.wav"),
        ]];
        assert_eq!(resolve_against(&saved, &buffers, 1), None);
    }

    #[test]
    fn a_group_count_mismatch_resets_the_whole_pick() {
        let buffers = open(&[(0, Some("/a.wav"), "a.wav")]);
        let saved = vec![vec![r(Some("/a.wav"), "a.wav")]];
        assert_eq!(resolve_against(&saved, &buffers, 2), None);
    }

    #[test]
    fn nothing_picked_resolves_to_empty_groups_rather_than_a_reset() {
        let buffers = open(&[(0, Some("/a.wav"), "a.wav")]);
        assert_eq!(
            resolve_against(&[Vec::new(), Vec::new()], &buffers, 2),
            Some(vec![Vec::new(), Vec::new()])
        );
    }

    /// The buffer was saved elsewhere (or reopened from a different directory) since the
    /// preset was written — the name is the only handle left, and using it beats resetting.
    #[test]
    fn a_moved_file_still_resolves_by_display_name() {
        let buffers = open(&[(0, Some("/new/place/one.wav"), "one.wav")]);
        let saved = vec![vec![r(Some("/old/place/one.wav"), "one.wav")]];
        assert_eq!(resolve_against(&saved, &buffers, 1), Some(vec![vec![0]]));
    }

    /// A never-saved scratch buffer has no path at all, on either side.
    #[test]
    fn a_pathless_buffer_resolves_by_name() {
        let buffers = open(&[(0, None, "untitled 2")]);
        let saved = vec![vec![r(None, "untitled 2")]];
        assert_eq!(resolve_against(&saved, &buffers, 1), Some(vec![vec![0]]));
    }

    /// Path beats name: two buffers share a display name, and the saved path picks the right
    /// one rather than the first one listed.
    #[test]
    fn path_wins_over_a_same_named_buffer_earlier_in_the_list() {
        let buffers = open(&[
            (0, Some("/take1/vox.wav"), "vox.wav"),
            (1, Some("/take2/vox.wav"), "vox.wav"),
        ]);
        let saved = vec![vec![r(Some("/take2/vox.wav"), "vox.wav")]];
        assert_eq!(resolve_against(&saved, &buffers, 1), Some(vec![vec![1]]));
    }
}
