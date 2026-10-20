//! Locality-biased target selection: every "pick an existing X" decision.
//!
//! Selection is a pure function of the rng and the model's sorted
//! `children` and per-node `counts`, so the same seed over the same model
//! yields the same picks.

use rand::distributions::{Distribution, WeightedIndex};
use rand::Rng;
use rand_chacha::ChaCha8Rng;

use crate::config::LocalityConfig;
use crate::model::{Model, Node, NodeId, NodeKind};

/// How many times [`pick_dest_dir`] redraws before settling for the root.
const DEST_RETRIES: usize = 16;

/// A uniformly chosen node of `kind` strictly below `from`, or `None` if
/// there is none. Walks down from `from`, choosing a child weighted by how
/// many `kind` nodes it is or holds, and stops at a `kind` child with
/// probability `1 / (1 + kind nodes below it)`. Never returns an
/// `OffLimits` node.
pub fn descend(
    rng: &mut ChaCha8Rng,
    model: &Model,
    from: NodeId,
    kind: NodeKind,
) -> Option<NodeId> {
    if kind == NodeKind::OffLimits {
        return None;
    }
    let k = kind.index();
    let mut cur = from;
    loop {
        let children = &model.node(cur).children;
        let weights: Vec<u64> = children
            .iter()
            .map(|&c| {
                let child = model.node(c);
                u64::from(child.kind == kind) + below(child, k)
            })
            .collect();
        // Fails only when every weight is zero: nothing of `kind` here.
        let dist = WeightedIndex::new(&weights).ok()?;
        let id = children[dist.sample(rng)];
        let child = model.node(id);
        if child.kind == kind {
            let deeper = below(child, k);
            if deeper == 0 || rng.gen_range(0..=deeper) == 0 {
                return Some(id);
            }
        }
        cur = id;
    }
}

/// A node of `kind` near `anchor`: with probability `cfg.same_dir` from
/// below `anchor`, `cfg.parent` from below its parent, else from below the
/// root, widening when the drawn scope holds none. Never the root.
pub fn pick(
    rng: &mut ChaCha8Rng,
    model: &Model,
    cfg: &LocalityConfig,
    anchor: NodeId,
    kind: NodeKind,
) -> Option<NodeId> {
    let first = draw_scope(rng, cfg);
    (first..SCOPES).find_map(|s| descend(rng, model, scope_root(model, anchor, s), kind))
}

/// A directory near `anchor` by the same scheme as [`pick`], where the
/// `same_dir` scope may also yield `anchor` itself (with probability
/// `1 / (1 + dirs below anchor)`), and the root is the final fallback.
pub fn pick_dir_or_root(
    rng: &mut ChaCha8Rng,
    model: &Model,
    cfg: &LocalityConfig,
    anchor: NodeId,
) -> NodeId {
    let first = draw_scope(rng, cfg);
    if first == 0 {
        let node = model.node(anchor);
        if node.kind == NodeKind::Dir {
            let deeper = below(node, NodeKind::Dir.index());
            if deeper == 0 || rng.gen_range(0..=deeper) == 0 {
                return anchor;
            }
        }
    }
    (first..SCOPES)
        .find_map(|s| descend(rng, model, scope_root(model, anchor, s), NodeKind::Dir))
        .unwrap_or(model.root())
}

/// Like [`pick_dir_or_root`], but never `not_inside` nor anything below
/// it; after [`DEST_RETRIES`] rejected draws, the root.
pub fn pick_dest_dir(
    rng: &mut ChaCha8Rng,
    model: &Model,
    cfg: &LocalityConfig,
    anchor: NodeId,
    not_inside: Option<NodeId>,
) -> NodeId {
    for _ in 0..DEST_RETRIES {
        let dir = pick_dir_or_root(rng, model, cfg, anchor);
        match not_inside {
            Some(x) if dir == x || model.is_ancestor(x, dir) => continue,
            _ => return dir,
        }
    }
    model.root()
}

/// The directory an operation on `id` touched: `id` if it is a `Dir`,
/// else its parent (the root if it has none).
pub fn anchor_of(model: &Model, id: NodeId) -> NodeId {
    let node = model.node(id);
    if node.kind == NodeKind::Dir {
        id
    } else {
        node.parent.unwrap_or(model.root())
    }
}

/// Number of locality scopes: anchor, parent, root.
const SCOPES: usize = 3;

/// One draw choosing the first scope to search. An all-zero config
/// (allowed by validation) means "anywhere".
fn draw_scope(rng: &mut ChaCha8Rng, cfg: &LocalityConfig) -> usize {
    match WeightedIndex::new([cfg.same_dir, cfg.parent, cfg.anywhere]) {
        Ok(dist) => dist.sample(rng),
        Err(_) => SCOPES - 1,
    }
}

fn scope_root(model: &Model, anchor: NodeId, scope: usize) -> NodeId {
    match scope {
        0 => anchor,
        1 => model.node(anchor).parent.unwrap_or(model.root()),
        _ => model.root(),
    }
}

/// Nodes of kind index `k` strictly below `node` (0 unless it is a `Dir`).
fn below(node: &Node, k: usize) -> u64 {
    if node.kind == NodeKind::Dir {
        u64::from(node.counts[k])
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn dir(m: &mut Model, parent: NodeId, name: &str) -> NodeId {
        m.insert(parent, name.into(), NodeKind::Dir, 0, 0)
    }

    fn file(m: &mut Model, parent: NodeId, name: &str) -> NodeId {
        m.insert(parent, name.into(), NodeKind::File, 1, 0)
    }

    fn only_same_dir() -> LocalityConfig {
        LocalityConfig {
            same_dir: 1.0,
            parent: 0.0,
            anywhere: 0.0,
        }
    }

    #[test]
    fn descend_is_uniform_over_kind() {
        // root/{a/{f1,f2,f3}, b/{f4}, f5}
        let mut m = Model::new();
        let root = m.root();
        let a = dir(&mut m, root, "a");
        let b = dir(&mut m, root, "b");
        let files = [
            file(&mut m, a, "f1"),
            file(&mut m, a, "f2"),
            file(&mut m, a, "f3"),
            file(&mut m, b, "f4"),
            file(&mut m, root, "f5"),
        ];
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut hits = [0u32; 5];
        for _ in 0..60_000 {
            let got = descend(&mut rng, &m, root, NodeKind::File).unwrap();
            let i = files.iter().position(|&f| f == got).expect("not a file");
            hits[i] += 1;
        }
        for (i, &h) in hits.iter().enumerate() {
            assert!(
                (10_200..=13_800).contains(&h),
                "file {i} drawn {h} times, want 12000 +-15%: {hits:?}"
            );
        }
    }

    #[test]
    fn descend_dir_is_uniform_and_excludes_from() {
        // root/{a/{a1/{a11}}, b}: dirs strictly below root are a, a1, a11, b.
        let mut m = Model::new();
        let root = m.root();
        let a = dir(&mut m, root, "a");
        let a1 = dir(&mut m, a, "a1");
        let a11 = dir(&mut m, a1, "a11");
        let b = dir(&mut m, root, "b");
        let dirs = [a, a1, a11, b];
        let mut rng = ChaCha8Rng::seed_from_u64(5);
        let mut hits = [0u32; 4];
        for _ in 0..40_000 {
            let got = descend(&mut rng, &m, root, NodeKind::Dir).unwrap();
            hits[dirs.iter().position(|&d| d == got).expect("not below root")] += 1;
        }
        for (i, &h) in hits.iter().enumerate() {
            assert!(
                (8_500..=11_500).contains(&h),
                "dir {i} drawn {h} times, want 10000 +-15%: {hits:?}"
            );
        }
        for _ in 0..1000 {
            let got = descend(&mut rng, &m, a, NodeKind::Dir).unwrap();
            assert!(got == a1 || got == a11);
        }
        assert_eq!(descend(&mut rng, &m, a11, NodeKind::Dir), None);
    }

    #[test]
    fn descend_none_when_kind_absent() {
        let mut m = Model::new();
        let root = m.root();
        let d = dir(&mut m, root, "d");
        let f = file(&mut m, d, "f");
        let locked = dir(&mut m, root, "locked");
        m.insert(locked, b"secret".to_vec(), NodeKind::OffLimits, 0, 0);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        assert_eq!(descend(&mut rng, &m, root, NodeKind::Symlink), None);
        assert_eq!(descend(&mut rng, &m, locked, NodeKind::File), None);
        assert_eq!(descend(&mut rng, &m, locked, NodeKind::Dir), None);
        // A file is not a directory: nothing below it.
        assert_eq!(descend(&mut rng, &m, f, NodeKind::File), None);
        // Off-limits nodes are never returned for any kind callers use.
        for _ in 0..1000 {
            assert_eq!(descend(&mut rng, &m, root, NodeKind::File), Some(f));
        }
    }

    #[test]
    fn pick_prefers_anchor_then_widens() {
        // root/{near/{n1}, far/{x1..x50}}
        let mut m = Model::new();
        let root = m.root();
        let near = dir(&mut m, root, "near");
        let n1 = file(&mut m, near, "n1");
        let far = dir(&mut m, root, "far");
        for i in 1..=50 {
            file(&mut m, far, &format!("x{i}"));
        }
        let cfg = only_same_dir();
        let mut rng = ChaCha8Rng::seed_from_u64(9);
        for _ in 0..1000 {
            assert_eq!(pick(&mut rng, &m, &cfg, near, NodeKind::File), Some(n1));
        }
        m.remove(n1);
        for _ in 0..1000 {
            let got = pick(&mut rng, &m, &cfg, near, NodeKind::File).expect("files exist");
            assert_eq!(m.node(got).parent, Some(far));
            assert_eq!(m.node(got).kind, NodeKind::File);
        }
        // Nothing of the kind anywhere: None.
        assert_eq!(pick(&mut rng, &m, &cfg, near, NodeKind::Symlink), None);
        // The root is never picked.
        for _ in 0..1000 {
            let got = pick(
                &mut rng,
                &m,
                &LocalityConfig::default(),
                near,
                NodeKind::Dir,
            )
            .expect("dirs exist");
            assert_ne!(got, root);
        }
    }

    #[test]
    fn pick_dir_or_root_falls_back_to_root() {
        let mut m = Model::new();
        let root = m.root();
        let mut rng = ChaCha8Rng::seed_from_u64(2);
        let cfg = LocalityConfig::default();
        for _ in 0..100 {
            assert_eq!(pick_dir_or_root(&mut rng, &m, &cfg, root), root);
        }
        let d = dir(&mut m, root, "d");
        let e = dir(&mut m, d, "e");
        // While directories exist the root is only a fallback, never drawn;
        // the anchor itself and its subdirectory are both reachable.
        let mut seen = [false; 2];
        for _ in 0..1000 {
            let got = pick_dir_or_root(&mut rng, &m, &cfg, d);
            seen[[d, e].iter().position(|&x| x == got).expect("d or e")] = true;
        }
        assert_eq!(seen, [true; 2], "anchor and its subdir both reachable");
    }

    #[test]
    fn pick_dest_dir_excludes_subtree() {
        // root/{d/{d1/{d11}, d2}, e/{e1}, f}
        let mut m = Model::new();
        let root = m.root();
        let d = dir(&mut m, root, "d");
        let d1 = dir(&mut m, d, "d1");
        dir(&mut m, d1, "d11");
        dir(&mut m, d, "d2");
        let e = dir(&mut m, root, "e");
        dir(&mut m, e, "e1");
        file(&mut m, root, "f");
        let cfg = LocalityConfig::default();
        let mut rng = ChaCha8Rng::seed_from_u64(11);
        let mut outside = 0;
        for anchor in [d, d1, e, root] {
            for _ in 0..1000 {
                let got = pick_dest_dir(&mut rng, &m, &cfg, anchor, Some(d));
                assert!(got != d && !m.is_ancestor(d, got), "picked {got} inside d");
                assert_eq!(m.node(got).kind, NodeKind::Dir);
                if got != root {
                    outside += 1;
                }
            }
        }
        assert!(outside > 0, "non-root destinations outside d are reachable");
        // Without an exclusion every result is still a directory.
        for _ in 0..1000 {
            let got = pick_dest_dir(&mut rng, &m, &cfg, d, None);
            assert_eq!(m.node(got).kind, NodeKind::Dir);
        }
    }

    #[test]
    fn pick_is_deterministic() {
        let mut m = Model::new();
        let root = m.root();
        let mut first = None;
        for i in 0..5 {
            let d = dir(&mut m, root, &format!("d{i}"));
            first.get_or_insert(d);
            for j in 0..=i * 3 {
                file(&mut m, d, &format!("f{j}"));
                let s = dir(&mut m, d, &format!("s{j}"));
                file(&mut m, s, "leaf");
            }
        }
        let cfg = LocalityConfig::default();
        let run = |seed: u64| {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let mut out = Vec::new();
            let mut anchor = root;
            for i in 0..500 {
                let got = match i % 3 {
                    0 => pick(&mut rng, &m, &cfg, anchor, NodeKind::File).unwrap(),
                    1 => pick_dir_or_root(&mut rng, &m, &cfg, anchor),
                    _ => pick_dest_dir(&mut rng, &m, &cfg, anchor, first),
                };
                anchor = anchor_of(&m, got);
                out.push(got);
            }
            out
        };
        assert_eq!(run(77), run(77));
        assert_ne!(run(77), run(78));
    }

    #[test]
    fn anchor_of_is_dir_or_parent() {
        let mut m = Model::new();
        let root = m.root();
        let d = dir(&mut m, root, "d");
        let f = file(&mut m, d, "f");
        assert_eq!(anchor_of(&m, d), d);
        assert_eq!(anchor_of(&m, f), d);
        assert_eq!(anchor_of(&m, root), root);
    }
}
