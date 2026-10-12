//! A model filesystem for tests (`FsView` over an in-memory tree) plus a small
//! harness that drives an [`Applier`] through its public surface.
//!
//! This is the "fake watcher backend" harness of §14.1: we inject arbitrary
//! `FsEvent` sequences (including events that *lie*) and then rescan, asserting
//! the index converges to the truth.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::applier::{DirItem, FsView, StatInfo};

/// One node in the model tree: a dir (with children) or a file.
#[derive(Clone, Debug)]
pub enum Node {
    Dir(BTreeMap<Vec<u8>, Node>),
    File,
    /// A file with an execute bit set (`find -perm /111`): the model's
    /// analogue of a `chmod +x`'d file, so crawl-diff tests can assert the
    /// `IS_EXEC` flag converges.
    ExecFile,
    Symlink,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        matches!(self, Node::Dir(_))
    }
    pub fn is_symlink(&self) -> bool {
        matches!(self, Node::Symlink)
    }
    pub fn is_exec(&self) -> bool {
        matches!(self, Node::ExecFile)
    }

    /// Build a directory node whose children are all plain files.
    pub fn dir<S: AsRef<[u8]>>(names: &[S]) -> Node {
        let mut m = BTreeMap::new();
        for n in names {
            m.insert(n.as_ref().to_vec(), Node::File);
        }
        Node::Dir(m)
    }
}

/// A model filesystem rooted at `/model`.
#[derive(Clone, Default)]
pub struct ModelFs {
    inner: Arc<Mutex<BTreeMap<Vec<u8>, Node>>>,
}

impl ModelFs {
    pub fn new() -> ModelFs {
        ModelFs {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with(entries: impl IntoIterator<Item = (&'static str, Node)>) -> ModelFs {
        let m = ModelFs::new();
        {
            let mut g = m.inner.lock().unwrap();
            for (k, v) in entries {
                g.insert(k.as_bytes().to_vec(), v);
            }
        }
        m
    }

    pub fn set_root_children(&self, entries: Vec<(Vec<u8>, Node)>) {
        let mut g = self.inner.lock().unwrap();
        *g = entries.into_iter().collect();
    }

    pub fn put(&self, name: &[u8], node: Node) {
        self.inner.lock().unwrap().insert(name.to_vec(), node);
    }

    pub fn remove(&self, name: &[u8]) {
        self.inner.lock().unwrap().remove(name);
    }

    /// Flatten to `path -> is_dir` for comparison, rooted at `prefix`.
    pub fn flatten(&self, prefix: &str) -> BTreeMap<String, bool> {
        let mut out = BTreeMap::new();
        let g = self.inner.lock().unwrap();
        for (name, node) in g.iter() {
            walk(&mut out, prefix, name, node);
        }
        out
    }
}

fn walk(out: &mut BTreeMap<String, bool>, prefix: &str, name: &[u8], node: &Node) {
    let path = format!("{prefix}/{}", String::from_utf8_lossy(name));
    match node {
        Node::Dir(children) => {
            out.insert(path.clone(), true);
            for (n, c) in children {
                walk(out, &path, n, c);
            }
        }
        Node::File => {
            out.insert(path, false);
        }
        // An executable file flattens exactly like a plain one: `flatten`
        // answers "what is where", and the exec bit is asserted separately
        // through the index flags.
        Node::ExecFile => {
            out.insert(path, false);
        }
        Node::Symlink => {
            out.insert(path, false);
        }
    }
}

impl FsView for ModelFs {
    fn read_dir(&self, abs_path: &Path) -> io::Result<Vec<DirItem>> {
        // The model is flat: `/model`, `/model/<dir>`, `/model/<dir>/<sub>`.
        // We resolve by stripping the `/model` prefix and walking components.
        // `Path::join` in the applier uses `\` on Windows, so normalize first
        // (the model's canonical form is `/`, like the index).
        let g = self.inner.lock().unwrap();
        let s = abs_path.to_string_lossy().replace('\\', "/");
        let rel = s
            .strip_prefix("/model")
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        if rel.is_empty() {
            return Ok(g
                .iter()
                .map(|(n, node)| DirItem {
                    name: n.clone(),
                    is_dir: node.is_dir(),
                    is_symlink: node.is_symlink(),
                    is_exec: node.is_exec(),
                })
                .collect());
        }
        // Walk components.
        let mut cur: &BTreeMap<Vec<u8>, Node> = &g;
        for comp in rel.split('/') {
            match cur.get(comp.as_bytes()) {
                Some(Node::Dir(children)) => cur = children,
                _ => return Ok(Vec::new()),
            }
        }
        Ok(cur
            .iter()
            .map(|(n, node)| DirItem {
                name: n.clone(),
                is_dir: node.is_dir(),
                is_symlink: node.is_symlink(),
                is_exec: node.is_exec(),
            })
            .collect())
    }

    fn stat(&self, _abs_path: &Path) -> io::Result<Option<StatInfo>> {
        // The model does not track inodes; identity is by path.
        Ok(None)
    }
}

/// Convenience helper: the model's root path.
pub fn model_root() -> PathBuf {
    PathBuf::from("/model")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_read_dir_root_and_sub() {
        let m = ModelFs::new();
        m.set_root_children(vec![
            (b"src".to_vec(), Node::dir(&[b"main.rs"])),
            (b"README.md".to_vec(), Node::File),
        ]);
        let root = m.read_dir(Path::new("/model")).unwrap();
        assert_eq!(root.len(), 2);
        let sub = m.read_dir(Path::new("/model/src")).unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, b"main.rs");
        assert!(!sub[0].is_dir);
    }

    #[test]
    fn flatten_paths() {
        let m = ModelFs::new();
        m.set_root_children(vec![
            (b"src".to_vec(), Node::dir(&[b"main.rs"])),
            (b"a.txt".to_vec(), Node::File),
        ]);
        let f = m.flatten("/model");
        assert!(f.contains_key("/model/src"));
        assert!(f.contains_key("/model/src/main.rs"));
        assert!(f.contains_key("/model/a.txt"));
        assert!(f["/model/src"]);
        assert!(!f["/model/a.txt"]);
    }
}
