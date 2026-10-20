//! In-memory arena model of the tree filegen mutates.
//!
//! The model is the only source for "pick an existing X" decisions. Nodes
//! store their own name and a parent link, never full paths, so memory grows
//! with entry count rather than path length.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::escape::escape;

/// First inode number handed out by [`Model::synthetic_ino`].
const SYNTHETIC_INO_BASE: u64 = 1 << 63;

/// Index of a node in the model's arena. The root is always 0.
pub type NodeId = u32;

/// What a node is. `OffLimits` entries are kept for name collisions and
/// subtree flags but never touched or descended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
    Fifo,
    Device,
    OffLimits,
}

impl NodeKind {
    /// Number of kinds; the length of [`Node::counts`].
    pub const COUNT: usize = 6;

    /// Position of this kind in [`Node::counts`].
    pub fn index(self) -> usize {
        self as usize
    }
}

/// One entry of the tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: Box<[u8]>,
    pub parent: Option<NodeId>,
    pub kind: NodeKind,
    pub size: u64,
    pub ino: u64,
    /// Sorted bytewise by name; non-empty only for `Dir`.
    pub children: Vec<NodeId>,
    /// `counts[k]` = number of nodes of kind `k` strictly below this node.
    pub counts: [u32; NodeKind::COUNT],
}

impl Node {
    fn new(name: Box<[u8]>, kind: NodeKind, size: u64, ino: u64) -> Node {
        Node {
            name,
            parent: None,
            kind,
            size,
            ino,
            children: Vec::new(),
            counts: [0; NodeKind::COUNT],
        }
    }
}

/// Arena of nodes plus a free list of reusable ids.
#[derive(Clone, Debug)]
pub struct Model {
    nodes: Vec<Node>,
    /// Per node: folded child name -> number of children folding to it.
    /// A multiset rather than a set, because a case-sensitive disk may hold
    /// two names that fold alike and removing one must not hide the other.
    folds: Vec<BTreeMap<String, u32>>,
    free: Vec<NodeId>,
    live: usize,
    total_bytes: u64,
    /// Next inode handed out by [`Model::synthetic_ino`].
    next_synthetic_ino: u64,
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

impl Model {
    /// A model holding only the root directory (id 0).
    pub fn new() -> Model {
        Model {
            nodes: vec![Node::new(Box::default(), NodeKind::Dir, 0, 0)],
            folds: vec![BTreeMap::new()],
            free: Vec::new(),
            live: 1,
            total_bytes: 0,
            next_synthetic_ino: SYNTHETIC_INO_BASE,
        }
    }

    pub fn root(&self) -> NodeId {
        0
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// Whether `id` names a live node: in range and either the root or
    /// attached to a parent (freed slots are parentless placeholders).
    pub fn is_live(&self, id: NodeId) -> bool {
        id == self.root()
            || self
                .nodes
                .get(id as usize)
                .is_some_and(|n| n.parent.is_some())
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id as usize]
    }

    /// Number of live nodes, root excluded.
    pub fn entries(&self) -> usize {
        self.live - 1
    }

    /// Sum of `size` over `File` nodes (each hard link counts again).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// A fresh, unique inode number for a file created in a dry run, where
    /// no real inode exists; hard links are keyed by inode, so they must
    /// not all share 0. Drawn from the top half of the range, away from
    /// real inode numbers.
    pub fn synthetic_ino(&mut self) -> u64 {
        let ino = self.next_synthetic_ino;
        self.next_synthetic_ino += 1;
        ino
    }

    /// Case- and normalization-insensitive key: NFD of the lowercased name.
    pub fn fold(name: &str) -> String {
        name.to_lowercase().nfd().collect()
    }

    /// Fold key of a byte name; non-UTF-8 names fold to their escaped form.
    fn fold_bytes(name: &[u8]) -> String {
        match std::str::from_utf8(name) {
            Ok(s) => Model::fold(s),
            Err(_) => escape(name),
        }
    }

    /// Path relative to the root; empty for the root itself.
    pub fn rel_path(&self, id: NodeId) -> Vec<u8> {
        let mut chain = Vec::new();
        let mut cur = id;
        while let Some(p) = self.node(cur).parent {
            chain.push(cur);
            cur = p;
        }
        let mut out = Vec::with_capacity(self.rel_path_len(id));
        for (i, &c) in chain.iter().rev().enumerate() {
            if i > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(&self.node(c).name);
        }
        out
    }

    pub fn abs_path(&self, root: &Path, id: NodeId) -> PathBuf {
        let rel = self.rel_path(id);
        if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(OsStr::from_bytes(&rel))
        }
    }

    /// Look up a `/`-separated relative path; `b""` is the root.
    pub fn resolve(&self, rel: &[u8]) -> Option<NodeId> {
        if rel.is_empty() {
            return Some(self.root());
        }
        rel.split(|&b| b == b'/')
            .try_fold(self.root(), |dir, name| self.child(dir, name))
    }

    fn child_pos(&self, dir: NodeId, name: &[u8]) -> Result<usize, usize> {
        self.node(dir)
            .children
            .binary_search_by(|&c| self.node(c).name.as_ref().cmp(name))
    }

    pub fn child(&self, dir: NodeId, name: &[u8]) -> Option<NodeId> {
        let pos = self.child_pos(dir, name).ok()?;
        Some(self.node(dir).children[pos])
    }

    /// Whether some child of `dir` folds to `folded` (see [`Model::fold`]).
    pub fn has_folded_name(&self, dir: NodeId, folded: &str) -> bool {
        self.folds[dir as usize].contains_key(folded)
    }

    /// Link the detached node `id` into `dir` under its current name and
    /// add its subtree counts to every ancestor. Panics on a duplicate name.
    fn attach(&mut self, dir: NodeId, id: NodeId) {
        assert_eq!(self.node(dir).kind, NodeKind::Dir, "parent is not a dir");
        let name = self.node(id).name.clone();
        let pos = match self.child_pos(dir, &name) {
            Ok(_) => panic!("duplicate name {} in dir {dir}", escape(&name)),
            Err(pos) => pos,
        };
        self.node_mut(dir).children.insert(pos, id);
        *self.folds[dir as usize]
            .entry(Model::fold_bytes(&name))
            .or_insert(0) += 1;
        self.node_mut(id).parent = Some(dir);
        let delta = self.delta(id);
        self.adjust_ancestors(dir, &delta, true);
    }

    /// Unlink `id` from its parent, keeping its own subtree intact.
    fn detach(&mut self, id: NodeId) {
        let dir = self.node(id).parent.expect("cannot detach the root");
        let name = self.node(id).name.clone();
        let pos = self
            .child_pos(dir, &name)
            .expect("child missing from parent");
        self.node_mut(dir).children.remove(pos);
        let folds = &mut self.folds[dir as usize];
        let key = Model::fold_bytes(&name);
        let n = folds.get_mut(&key).expect("fold key missing");
        *n -= 1;
        if *n == 0 {
            folds.remove(&key);
        }
        let delta = self.delta(id);
        self.adjust_ancestors(dir, &delta, false);
        self.node_mut(id).parent = None;
    }

    /// Counts that `id` and its subtree contribute to each ancestor.
    fn delta(&self, id: NodeId) -> [u32; NodeKind::COUNT] {
        let n = self.node(id);
        let mut d = n.counts;
        d[n.kind.index()] += 1;
        d
    }

    /// Add (or subtract) `delta` on `from` and every ancestor of it.
    fn adjust_ancestors(&mut self, from: NodeId, delta: &[u32; NodeKind::COUNT], add: bool) {
        let mut cur = Some(from);
        while let Some(a) = cur {
            let node = self.node_mut(a);
            for (c, d) in node.counts.iter_mut().zip(delta) {
                if add {
                    *c += d;
                } else {
                    *c -= d;
                }
            }
            cur = node.parent;
        }
    }

    /// Add a node under `dir`. Panics if `dir` already has a child `name`
    /// or is not a directory.
    pub fn insert(
        &mut self,
        dir: NodeId,
        name: Vec<u8>,
        kind: NodeKind,
        size: u64,
        ino: u64,
    ) -> NodeId {
        let node = Node::new(name.into_boxed_slice(), kind, size, ino);
        let id = match self.free.pop() {
            Some(id) => {
                self.nodes[id as usize] = node;
                id
            }
            None => {
                self.nodes.push(node);
                self.folds.push(BTreeMap::new());
                NodeId::try_from(self.nodes.len() - 1).expect("too many nodes")
            }
        };
        self.attach(dir, id);
        self.live += 1;
        if kind == NodeKind::File {
            self.total_bytes += size;
        }
        id
    }

    /// Remove a leaf or an empty directory.
    pub fn remove(&mut self, id: NodeId) {
        assert!(
            self.node(id).children.is_empty(),
            "remove of a non-empty dir"
        );
        self.remove_subtree(id);
    }

    /// Remove `id` and everything below it; returns the number removed.
    pub fn remove_subtree(&mut self, id: NodeId) -> u32 {
        assert_ne!(id, self.root(), "cannot remove the root");
        self.detach(id);
        let mut removed = 0;
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let node = std::mem::replace(
                &mut self.nodes[n as usize],
                // A dead slot: off-limits and parentless, so a stale id is
                // never mistaken for a live directory.
                Node::new(Box::default(), NodeKind::OffLimits, 0, 0),
            );
            if node.kind == NodeKind::File {
                self.total_bytes -= node.size;
            }
            stack.extend(node.children);
            self.folds[n as usize].clear();
            self.free.push(n);
            self.live -= 1;
            removed += 1;
        }
        removed
    }

    /// Move `id` with its subtree to `new_dir` under `new_name`. Panics if
    /// `new_dir` is `id` or below it, or if `new_name` is taken there.
    pub fn rename(&mut self, id: NodeId, new_dir: NodeId, new_name: Vec<u8>) {
        assert!(
            id != new_dir && !self.is_ancestor(id, new_dir),
            "rename into own subtree"
        );
        self.detach(id);
        self.node_mut(id).name = new_name.into_boxed_slice();
        self.attach(new_dir, id);
    }

    pub fn set_size(&mut self, id: NodeId, size: u64) {
        let node = &mut self.nodes[id as usize];
        if node.kind == NodeKind::File {
            self.total_bytes = self.total_bytes - node.size + size;
        }
        node.size = size;
    }

    /// Whether `a` is a strict ancestor of `b`.
    pub fn is_ancestor(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = self.node(b).parent;
        while let Some(p) = cur {
            if p == a {
                return true;
            }
            cur = self.node(p).parent;
        }
        false
    }

    /// Number of path components from the root (root is 0).
    pub fn depth(&self, id: NodeId) -> u32 {
        let mut d = 0;
        let mut cur = self.node(id).parent;
        while let Some(p) = cur {
            d += 1;
            cur = self.node(p).parent;
        }
        d
    }

    /// Byte length of [`Model::rel_path`].
    pub fn rel_path_len(&self, id: NodeId) -> usize {
        let mut len = 0;
        let mut cur = id;
        while let Some(p) = self.node(cur).parent {
            len += self.node(cur).name.len();
            if p != self.root() {
                len += 1;
            }
            cur = p;
        }
        len
    }

    /// Maximum [`Model::rel_path_len`] over `id` and everything below it.
    pub fn max_rel_path_len_in(&self, id: NodeId) -> usize {
        let base = self.rel_path_len(id);
        let mut max = base;
        let mut stack = vec![(id, base)];
        while let Some((n, len)) = stack.pop() {
            let sep = usize::from(n != self.root());
            for &c in &self.node(n).children {
                let clen = len + sep + self.node(c).name.len();
                max = max.max(clen);
                stack.push((c, clen));
            }
        }
        max
    }

    /// Number of nodes strictly below `id`.
    pub fn subtree_entries(&self, id: NodeId) -> u32 {
        self.node(id).counts.iter().sum()
    }

    pub fn has_off_limits_below(&self, id: NodeId) -> bool {
        self.node(id).counts[NodeKind::OffLimits.index()] > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(m: &Model, dir: NodeId) -> Vec<&[u8]> {
        m.node(dir)
            .children
            .iter()
            .map(|&c| m.node(c).name.as_ref())
            .collect()
    }

    #[test]
    fn synthetic_inos_are_unique_and_high() {
        let mut m = Model::new();
        let a = m.synthetic_ino();
        let b = m.synthetic_ino();
        assert_eq!(a, 1 << 63);
        assert_eq!(b, a + 1);
    }

    #[test]
    fn insert_keeps_children_sorted_and_counts() {
        let mut m = Model::new();
        let r = m.root();
        let d = m.insert(r, b"b".to_vec(), NodeKind::Dir, 0, 0);
        let f = m.insert(d, b"z".to_vec(), NodeKind::File, 10, 1);
        m.insert(r, b"a".to_vec(), NodeKind::File, 5, 2);
        assert_eq!(names(&m, r), [&b"a"[..], b"b"]);
        assert_eq!(m.node(r).counts[NodeKind::File.index()], 2);
        assert_eq!(m.node(d).counts[NodeKind::File.index()], 1);
        assert_eq!(m.total_bytes(), 15);
        assert_eq!(m.rel_path(f), b"b/z");
        assert_eq!(m.resolve(b"b/z"), Some(f));
        assert_eq!(m.resolve(b""), Some(r));
        assert_eq!(m.resolve(b"b/missing"), None);
        assert_eq!(m.child(r, b"b"), Some(d));
        assert_eq!(m.entries(), 3);
        assert_eq!(m.abs_path(Path::new("/t"), f), PathBuf::from("/t/b/z"));
        assert_eq!(m.abs_path(Path::new("/t"), r), PathBuf::from("/t"));
    }

    #[test]
    #[should_panic]
    fn insert_duplicate_name_panics() {
        let mut m = Model::new();
        let r = m.root();
        m.insert(r, b"a".to_vec(), NodeKind::File, 0, 0);
        m.insert(r, b"a".to_vec(), NodeKind::Dir, 0, 0);
    }

    #[test]
    fn rename_moves_counts_and_subtree() {
        let mut m = Model::new();
        let r = m.root();
        let d1 = m.insert(r, b"d1".to_vec(), NodeKind::Dir, 0, 0);
        let f = m.insert(d1, b"f".to_vec(), NodeKind::File, 3, 1);
        let d2 = m.insert(r, b"d2".to_vec(), NodeKind::Dir, 0, 0);
        m.rename(d1, d2, b"x".to_vec());
        assert_eq!(m.node(d2).counts[NodeKind::Dir.index()], 1);
        assert_eq!(m.node(d2).counts[NodeKind::File.index()], 1);
        assert_eq!(m.node(r).counts[NodeKind::Dir.index()], 2);
        assert_eq!(m.node(r).counts[NodeKind::File.index()], 1);
        assert_eq!(m.rel_path(f), b"d2/x/f");
        assert!(m.is_ancestor(d2, f));
        assert!(!m.is_ancestor(f, d2));
        assert!(!m.is_ancestor(d2, d2));
        assert_eq!(names(&m, r), [&b"d2"[..]]);
        assert_eq!(m.child(d2, b"x"), Some(d1));
        assert!(m.has_folded_name(d2, &Model::fold("X")));
        assert!(!m.has_folded_name(r, &Model::fold("d1")));
        assert_eq!(m.depth(f), 3);
        // Rename within the same directory keeps counts and lookup.
        m.rename(f, d1, b"a".to_vec());
        assert_eq!(m.node(d1).counts[NodeKind::File.index()], 1);
        assert_eq!(m.resolve(b"d2/x/a"), Some(f));
        assert!(!m.has_folded_name(d1, &Model::fold("f")));
        assert_eq!(m.total_bytes(), 3);
    }

    #[test]
    fn freed_slots_are_not_live() {
        let mut m = Model::new();
        let r = m.root();
        assert!(m.is_live(r));
        let d = m.insert(r, b"d".to_vec(), NodeKind::Dir, 0, 0);
        let f = m.insert(d, b"f".to_vec(), NodeKind::File, 1, 1);
        assert!(m.is_live(d) && m.is_live(f));
        assert!(!m.is_live(f + 100));
        m.remove_subtree(d);
        for id in [d, f] {
            assert!(!m.is_live(id));
            let n = m.node(id);
            assert_eq!(n.kind, NodeKind::OffLimits);
            assert_eq!(n.parent, None);
            assert!(n.name.is_empty() && n.children.is_empty());
            assert_eq!(n.counts, [0; NodeKind::COUNT]);
        }
        let g = m.insert(r, b"g".to_vec(), NodeKind::File, 1, 2);
        assert!(m.is_live(g));
    }

    #[test]
    fn remove_subtree_returns_count_and_frees_bytes() {
        let mut m = Model::new();
        let r = m.root();
        let d = m.insert(r, b"d".to_vec(), NodeKind::Dir, 0, 0);
        m.insert(d, b"f1".to_vec(), NodeKind::File, 4, 1);
        m.insert(d, b"f2".to_vec(), NodeKind::File, 6, 2);
        assert_eq!(m.subtree_entries(d), 2);
        assert_eq!(m.remove_subtree(d), 3);
        assert_eq!(m.total_bytes(), 0);
        assert_eq!(m.entries(), 0);
        assert_eq!(m.node(r).counts, [0; NodeKind::COUNT]);
        assert!(m.node(r).children.is_empty());
        assert!(!m.has_folded_name(r, &Model::fold("d")));
        // Freed ids are reused and the model stays consistent.
        let g = m.insert(r, b"g".to_vec(), NodeKind::File, 1, 3);
        assert_eq!(m.rel_path(g), b"g");
        assert_eq!(m.entries(), 1);
        m.remove(g);
        assert_eq!(m.entries(), 0);
        assert_eq!(m.total_bytes(), 0);
    }

    #[test]
    fn set_size_updates_total_bytes() {
        let mut m = Model::new();
        let r = m.root();
        let f = m.insert(r, b"f".to_vec(), NodeKind::File, 4, 1);
        m.set_size(f, 9);
        assert_eq!(m.node(f).size, 9);
        assert_eq!(m.total_bytes(), 9);
    }

    #[test]
    fn folded_names_collide() {
        let mut m = Model::new();
        let r = m.root();
        m.insert(r, "Été.txt".as_bytes().to_vec(), NodeKind::File, 0, 0);
        assert!(m.has_folded_name(r, &Model::fold("été.txt")));
        assert!(m.has_folded_name(r, &Model::fold("E\u{301}te\u{301}.txt")));
        assert!(!m.has_folded_name(r, &Model::fold("ete.txt")));
        // Two names folding alike (possible on a case-sensitive disk):
        // removing one keeps the fold key alive for the other.
        let lower = m.insert(r, "été.txt".as_bytes().to_vec(), NodeKind::File, 0, 0);
        m.remove(lower);
        assert!(m.has_folded_name(r, &Model::fold("ÉTÉ.TXT")));
    }

    #[test]
    fn path_len_and_depth() {
        let mut m = Model::new();
        let r = m.root();
        let a = m.insert(r, b"a".to_vec(), NodeKind::Dir, 0, 0);
        let bb = m.insert(a, b"bb".to_vec(), NodeKind::Dir, 0, 0);
        let ccc = m.insert(bb, b"ccc".to_vec(), NodeKind::File, 0, 0);
        assert_eq!(m.depth(r), 0);
        assert_eq!(m.depth(ccc), 3);
        assert_eq!(m.rel_path_len(r), 0);
        assert_eq!(m.rel_path_len(ccc), 8);
        assert_eq!(m.max_rel_path_len_in(a), 8);
        assert_eq!(m.max_rel_path_len_in(r), 8);
        assert_eq!(m.max_rel_path_len_in(ccc), 8);
        assert_eq!(m.subtree_entries(a), 2);
        assert!(!m.has_off_limits_below(r));
        m.insert(bb, b".fileset".to_vec(), NodeKind::OffLimits, 0, 0);
        assert!(m.has_off_limits_below(r) && m.has_off_limits_below(a));
        assert!(!m.has_off_limits_below(ccc));
    }
}
