use std::collections::BTreeMap;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatTreeRowKind {
    Directory,
    File { file_index: usize },
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
    pub fn build(inputs: &[FileTreeInput<'_>]) -> Self {
        let mut root = TreeNode::default();
        for input in inputs {
            root.insert(input);
        }

        let mut rows = Vec::new();
        root.flatten(0, &mut rows);
        Self { rows }
    }

    pub fn selected_row_for_file(&self, file_index: usize) -> Option<usize> {
        self.rows.iter().position(|row| {
            matches!(row.kind, FlatTreeRowKind::File { file_index: index } if index == file_index)
        })
    }

    pub fn next_file_index(&self, current_file_index: usize, delta: isize) -> Option<usize> {
        let file_rows: Vec<_> = self
            .rows
            .iter()
            .filter_map(|row| match row.kind {
                FlatTreeRowKind::Directory => None,
                FlatTreeRowKind::File { file_index } => Some(file_index),
            })
            .collect();
        if file_rows.is_empty() {
            return None;
        }

        let current_position = file_rows
            .iter()
            .position(|index| *index == current_file_index)
            .unwrap_or(0);
        let max = file_rows.len() as isize - 1;
        let next_position = (current_position as isize + delta).clamp(0, max) as usize;
        Some(file_rows[next_position])
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

    fn flatten(&self, depth: usize, rows: &mut Vec<FlatTreeRow>) {
        for (name, child) in &self.directories {
            let path = if depth == 0 {
                name.clone()
            } else {
                child
                    .first_file_path()
                    .and_then(|path| {
                        path.rsplit_once('/')
                            .map(|(directory, _)| directory.to_owned())
                    })
                    .unwrap_or_else(|| name.clone())
            };
            rows.push(FlatTreeRow {
                depth,
                path,
                label: name.clone(),
                kind: FlatTreeRowKind::Directory,
                stats: child.stats,
            });
            child.flatten(depth + 1, rows);
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

    fn first_file_path(&self) -> Option<&str> {
        self.files
            .first()
            .map(|file| file.path.as_str())
            .or_else(|| {
                self.directories
                    .values()
                    .find_map(|child| child.first_file_path())
            })
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
        let tree = FileTreeView::build(&[
            input(0, "src/app.rs", false),
            input(1, "src/tui.rs", false),
            input(2, "README.md", false),
        ]);

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
        let tree =
            FileTreeView::build(&[input(0, "src/app.rs", true), input(1, "src/tui.rs", false)]);

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
        let tree =
            FileTreeView::build(&[input(0, "Cargo.toml", false), input(1, "README.md", true)]);

        assert_eq!(tree.rows.len(), 2);
        assert!(matches!(tree.rows[0].kind, FlatTreeRowKind::File { .. }));
        assert_eq!(tree.rows[0].depth, 0);
    }

    #[test]
    fn maps_selected_file_to_flattened_row() {
        let tree =
            FileTreeView::build(&[input(0, "src/app.rs", false), input(1, "README.md", false)]);

        assert_eq!(tree.selected_row_for_file(0), Some(1));
        assert_eq!(tree.selected_row_for_file(1), Some(2));
    }

    #[test]
    fn moves_selection_skipping_directory_rows() {
        let tree =
            FileTreeView::build(&[input(0, "src/app.rs", false), input(1, "README.md", false)]);

        assert_eq!(tree.next_file_index(0, 1), Some(1));
        assert_eq!(tree.next_file_index(1, -1), Some(0));
    }

    #[test]
    fn empty_tree_is_safe() {
        let tree = FileTreeView::build(&[]);

        assert!(tree.rows.is_empty());
        assert_eq!(tree.selected_row_for_file(0), None);
        assert_eq!(tree.next_file_index(0, 1), None);
    }
}
