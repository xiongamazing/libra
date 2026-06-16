//! Worktree helpers shared across commands.

use std::path::{Path, PathBuf};

use git_internal::internal::index::Index;

use crate::utils::{
    ignore::{self, IgnorePolicy},
    util,
};

/// Returns a list of paths in the working directory that are not tracked in the index.
///
/// This function lists all files in the working directory, filters them based on ignore rules
/// (like .gitignore), and checks if they are present in the index.
///
/// # Arguments
///
/// * `current_index` - The git index to check against
///
/// # Returns
///
/// A vector of paths that exist in the working directory but are not tracked in the index.
///
/// # Errors
///
/// Returns an error if:
/// * Failed to list working directory files
/// * A path contains invalid UTF-8
pub fn untracked_workdir_paths(current_index: &Index) -> Result<Vec<PathBuf>, String> {
    let workdir_files = util::list_workdir_files().map_err(|e| e.to_string())?;
    let visible_files =
        ignore::filter_workdir_paths(workdir_files, IgnorePolicy::Respect, current_index);
    let mut untracked = Vec::new();
    for path in visible_files {
        let path_str = path
            .to_str()
            .ok_or_else(|| format!("path {:?} is not valid UTF-8", path))?;
        if !index_has_any_stage(current_index, path_str) {
            untracked.push(path);
        }
    }
    Ok(untracked)
}

/// Checks if a path is present in the index at any stage (0-3).
pub fn index_has_any_stage(index: &Index, path: &str) -> bool {
    (0..=3).any(|stage| index.tracked(path, stage))
}

/// Checks if any untracked files would be overwritten by files in the new index.
///
/// Returns the first untracked path that conflicts with a tracked path in the new index.
///
/// # Arguments
///
/// * `untracked` - List of untracked file paths in the working directory
/// * `new_index` - The new index to check against
///
/// # Returns
///
/// The first untracked path that would be overwritten, or `None` if no conflicts exist.
///
/// # Example
///
/// ```ignore
/// let untracked = untracked_workdir_paths(&current_index)?;
/// if let Some(conflicting_path) = untracked_overwrite_path(&untracked, &new_index) {
///     return Err(format!("Untracked file would be overwritten: {:?}", conflicting_path));
/// }
/// ```
pub fn untracked_overwrite_path(untracked: &[PathBuf], new_index: &Index) -> Option<PathBuf> {
    let new_tracked = new_index.tracked_files();
    for untracked_path in untracked {
        for tracked_path in &new_tracked {
            if paths_conflict(untracked_path, tracked_path) {
                return Some(untracked_path.clone());
            }
        }
    }
    None
}

/// Determines if two paths conflict with each other.
///
/// A conflict occurs if:
/// - The paths are identical, or
/// - One path is an ancestor directory of the other (e.g., "foo" conflicts with "foo/bar")
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use libra::utils::worktree::paths_conflict;
///
/// assert!(paths_conflict(Path::new("foo"), Path::new("foo"))); // Identical
/// assert!(paths_conflict(Path::new("foo"), Path::new("foo/bar"))); // Parent/child
/// assert!(paths_conflict(Path::new("foo/bar"), Path::new("foo"))); // Child/parent
/// assert!(!paths_conflict(Path::new("foo"), Path::new("bar"))); // Unrelated
/// ```
pub fn paths_conflict(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use git_internal::{hash::ObjectHash, internal::index::Index};

    use super::*;

    // ── paths_conflict ───────────────────────────────────────────────

    #[test]
    fn paths_conflict_identical() {
        assert!(paths_conflict(Path::new("foo"), Path::new("foo")));
    }

    #[test]
    fn paths_conflict_parent_child() {
        assert!(paths_conflict(Path::new("foo"), Path::new("foo/bar")));
        assert!(paths_conflict(Path::new("foo/bar"), Path::new("foo")));
    }

    #[test]
    fn paths_conflict_deep_nesting() {
        assert!(paths_conflict(Path::new("a/b"), Path::new("a/b/c/d"),));
    }

    #[test]
    fn paths_conflict_unrelated() {
        assert!(!paths_conflict(Path::new("foo"), Path::new("bar")));
        assert!(!paths_conflict(Path::new("src/a"), Path::new("src/b")));
    }

    #[test]
    fn paths_conflict_similar_prefix_not_ancestor() {
        // "foobar" is not a child of "foo" — there is no path separator
        assert!(!paths_conflict(Path::new("foo"), Path::new("foobar"),));
    }

    #[test]
    fn paths_conflict_root_path() {
        assert!(paths_conflict(Path::new(""), Path::new("")));
    }

    // ── index_has_any_stage ──────────────────────────────────────────

    #[test]
    fn index_has_any_stage_finds_stage_zero() {
        let mut index = Index::new();
        let hash = ObjectHash::new(b"test");
        index.add(git_internal::internal::index::IndexEntry::new_from_blob(
            "src/main.rs".to_string(),
            hash,
            0,
        ));
        assert!(index_has_any_stage(&index, "src/main.rs"));
    }

    #[test]
    fn index_has_any_stage_returns_false_for_absent_path() {
        let index = Index::new();
        assert!(!index_has_any_stage(&index, "no/such/file.rs"));
    }

    // ── untracked_overwrite_path ─────────────────────────────────────

    #[test]
    fn untracked_overwrite_detects_exact_match() {
        let mut new_index = Index::new();
        let hash = ObjectHash::new(b"data");
        new_index.add(git_internal::internal::index::IndexEntry::new_from_blob(
            "readme.md".to_string(),
            hash,
            0,
        ));

        let untracked = vec![PathBuf::from("readme.md")];
        let conflict = untracked_overwrite_path(&untracked, &new_index);
        assert_eq!(conflict, Some(PathBuf::from("readme.md")));
    }

    #[test]
    fn untracked_overwrite_returns_none_when_no_conflict() {
        let mut new_index = Index::new();
        let hash = ObjectHash::new(b"data");
        new_index.add(git_internal::internal::index::IndexEntry::new_from_blob(
            "src/lib.rs".to_string(),
            hash,
            0,
        ));

        let untracked = vec![PathBuf::from("other_file.txt")];
        assert!(untracked_overwrite_path(&untracked, &new_index).is_none());
    }
}
