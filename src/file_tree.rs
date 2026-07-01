use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ViewedStats {
    pub viewed: usize,
    pub total: usize,
}

impl ViewedStats {
    pub fn mark(&self) -> &'static str {
        match (self.viewed, self.total) {
            (0, _) => "•",
            (viewed, total) if viewed == total => "✓",
            _ => "◐",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTreeInput<'a> {
    pub index: usize,
    pub path: &'a str,
    pub viewed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTreeView {
    pub rows: Vec<FlatTreeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatTreeRow {
    pub depth: usize,
    pub path: String,
    pub label: String,
    pub kind: FlatTreeRowKind,
    pub stats: ViewedStats,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TreeRowId {
    Directory(String),
    File { file_index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatTreeRowKind {
    Directory { collapsed: bool },
    File { file_index: usize },
}

impl FlatTreeRow {
    pub fn id(&self) -> TreeRowId {
        match self.kind {
            FlatTreeRowKind::Directory { .. } => TreeRowId::Directory(self.path.clone()),
            FlatTreeRowKind::File { file_index } => TreeRowId::File { file_index },
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TreeNode {
    directories: BTreeMap<String, TreeNode>,
    files: Vec<FileLeaf>,
    stats: ViewedStats,
}

#[derive(Debug, Clone)]
struct FileLeaf {
    index: usize,
    name: String,
    path: String,
    viewed: bool,
}

impl FileTreeView {
    pub fn build(inputs: &[FileTreeInput<'_>], collapsed_dirs: &BTreeSet<String>) -> Self {
        let mut root = TreeNode::default();
        for input in inputs {
            root.insert(input);
        }

        let mut rows = Vec::new();
        root.flatten(0, "", &mut rows, collapsed_dirs);
        Self { rows }
    }

    pub fn row_for_id(&self, id: &TreeRowId) -> Option<usize> {
        self.rows.iter().position(|row| &row.id() == id)
    }

    pub fn next_row_index(&self, current: Option<usize>, delta: isize) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let current = current.unwrap_or(0);
        let max = self.rows.len() as isize - 1;
        Some((current as isize + delta).clamp(0, max) as usize)
    }

    pub fn selected_row_for_file(&self, file_index: usize) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(row.kind, FlatTreeRowKind::File { file_index: index } if index == file_index)
        })
    }
}

impl TreeNode {
    fn insert(&mut self, input: &FileTreeInput<'_>) {
        self.stats.total += 1;
        if input.viewed {
            self.stats.viewed += 1;
        }

        let parts: Vec<_> = input
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let Some((filename, directories)) = parts.split_last() else {
            return;
        };

        let mut node = self;
        let mut directory_path = String::new();
        for directory in directories {
            if !directory_path.is_empty() {
                directory_path.push('/');
            }
            directory_path.push_str(directory);

            node = node.directories.entry((*directory).to_owned()).or_default();
            node.stats.total += 1;
            if input.viewed {
                node.stats.viewed += 1;
            }
        }

        node.files.push(FileLeaf {
            index: input.index,
            name: (*filename).to_owned(),
            path: input.path.to_owned(),
            viewed: input.viewed,
        });
        node.files.sort_by(|left, right| left.name.cmp(&right.name));
    }

    fn flatten(
        &self,
        depth: usize,
        parent_path: &str,
        rows: &mut Vec<FlatTreeRow>,
        collapsed_dirs: &BTreeSet<String>,
    ) {
        for (name, child) in &self.directories {
            let path = if depth == 0 {
                name.clone()
            } else {
                format!("{parent_path}/{name}")
            };
            let collapsed = collapsed_dirs.contains(&path);
            rows.push(FlatTreeRow {
                depth,
                path: path.clone(),
                label: name.clone(),
                kind: FlatTreeRowKind::Directory { collapsed },
                stats: child.stats,
            });
            if !collapsed {
                child.flatten(depth + 1, &path, rows, collapsed_dirs);
            }
        }

        for file in &self.files {
            rows.push(FlatTreeRow {
                depth,
                path: file.path.clone(),
                label: file.name.clone(),
                kind: FlatTreeRowKind::File {
                    file_index: file.index,
                },
                stats: ViewedStats {
                    viewed: usize::from(file.viewed),
                    total: 1,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(index: usize, path: &'static str, viewed: bool) -> FileTreeInput<'static> {
        FileTreeInput {
            index,
            path,
            viewed,
        }
    }

    #[test]
    fn builds_nested_tree_rows() {
        let tree = FileTreeView::build(
            &[
                input(0, "src/app.rs", false),
                input(1, "src/tui.rs", false),
                input(2, "README.md", false),
            ],
            &BTreeSet::new(),
        );

        let labels: Vec<_> = tree
            .rows
            .iter()
            .map(|row| (row.depth, row.label.as_str()))
            .collect();
        assert_eq!(
            labels,
            [(0, "src"), (1, "app.rs"), (1, "tui.rs"), (0, "README.md")]
        );
    }

    #[test]
    fn computes_directory_viewed_counts() {
        let tree = FileTreeView::build(
            &[input(0, "src/app.rs", true), input(1, "src/tui.rs", false)],
            &BTreeSet::new(),
        );

        assert_eq!(
            tree.rows[0].stats,
            ViewedStats {
                viewed: 1,
                total: 2
            }
        );
        assert_eq!(tree.rows[0].stats.mark(), "◐");
    }

    #[test]
    fn handles_root_level_files() {
        let tree = FileTreeView::build(
            &[input(0, "Cargo.toml", false), input(1, "README.md", true)],
            &BTreeSet::new(),
        );

        assert_eq!(tree.rows.len(), 2);
        assert!(matches!(tree.rows[0].kind, FlatTreeRowKind::File { .. }));
        assert_eq!(tree.rows[0].depth, 0);
    }

    #[test]
    fn maps_selected_file_to_flattened_row() {
        let tree = FileTreeView::build(
            &[input(0, "src/app.rs", false), input(1, "README.md", false)],
            &BTreeSet::new(),
        );

        assert_eq!(tree.selected_row_for_file(0), Some(1));
        assert_eq!(tree.selected_row_for_file(1), Some(2));
    }

    #[test]
    fn empty_tree_is_safe() {
        let tree = FileTreeView::build(&[], &BTreeSet::new());

        assert!(tree.rows.is_empty());
        assert_eq!(tree.selected_row_for_file(0), None);
        assert_eq!(tree.next_row_index(None, 1), None);
    }

    #[test]
    fn collapsed_directory_hides_descendants_but_keeps_stats() {
        let tree = FileTreeView::build(
            &[
                input(0, "src/app.rs", true),
                input(1, "src/tui.rs", false),
                input(2, "README.md", false),
            ],
            &BTreeSet::from(["src".to_owned()]),
        );

        let labels: Vec<_> = tree.rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["src", "README.md"]);
        assert!(matches!(
            tree.rows[0].kind,
            FlatTreeRowKind::Directory { collapsed: true }
        ));
        assert_eq!(
            tree.rows[0].stats,
            ViewedStats {
                viewed: 1,
                total: 2
            }
        );
    }

    #[test]
    fn nested_collapse_hides_only_matching_subtree() {
        let tree = FileTreeView::build(
            &[
                input(0, "src/ui/tui.rs", false),
                input(1, "src/app.rs", false),
                input(2, "tests/app_test.rs", false),
            ],
            &BTreeSet::from(["src/ui".to_owned()]),
        );

        let rows: Vec<_> = tree
            .rows
            .iter()
            .map(|row| (row.depth, row.path.as_str(), row.label.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (0, "src", "src"),
                (1, "src/ui", "ui"),
                (1, "src/app.rs", "app.rs"),
                (0, "tests", "tests"),
                (1, "tests/app_test.rs", "app_test.rs")
            ]
        );
    }

    #[test]
    fn row_identity_lookup_respects_collapsed_visibility() {
        let tree = FileTreeView::build(
            &[input(0, "src/app.rs", false), input(1, "README.md", false)],
            &BTreeSet::from(["src".to_owned()]),
        );

        assert_eq!(
            tree.row_for_id(&TreeRowId::Directory("src".to_owned())),
            Some(0)
        );
        assert_eq!(tree.row_for_id(&TreeRowId::File { file_index: 0 }), None);
        assert_eq!(tree.row_for_id(&TreeRowId::File { file_index: 1 }), Some(1));
    }
}
