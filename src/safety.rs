//! Colouring the chart by safety: each folder a finding covers (see `findings`) in the
//! colour of what the finding says about deleting it, and everything inside that folder
//! too. Everything else is a neutral grey. No finding says anything about it, which doesn't
//! make it safe. A folder that only holds findings stays grey as well: only the findings'
//! own folders, and what's in them, are coloured.

use std::collections::HashMap;

use crate::findings::{Finding, Fix, Safety};
use crate::scan::Tree;

pub const SAFE: u32 = 0x3fb950;
pub const REVIEW: u32 = 0xd29922;
/// A plain grey, not a muted green: nothing is known about it.
pub const NEUTRAL: u32 = 0x5c5f66;

/// The colour for a finding's verdict, or for what no finding covers (`None`). A new kind of
/// verdict (a "Manage in app" one, say) needs a colour here and a line in `LEGEND`; the
/// compiler points to this match.
pub fn color(safety: Option<Safety>) -> u32 {
    match safety {
        Some(Safety::Safe) => SAFE,
        Some(Safety::Review) => REVIEW,
        None => NEUTRAL,
    }
}

/// How the findings list and the chart's legend name a verdict.
pub fn label(safety: Safety) -> &'static str {
    match safety {
        Safety::Safe => "Safe to delete",
        Safety::Review => "Review first",
    }
}

/// What the chart's legend explains, in order.
pub const LEGEND: [(Option<Safety>, &str); 3] =
    [(Some(Safety::Safe), "Safe to delete"), (Some(Safety::Review), "Review first"), (None, "No finding")];

/// What a finding says about a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub safety: Safety,
    /// The finding's title, for the chart's hover text.
    pub title: &'static str,
}

/// Which finding covers which folder, made once from the findings and looked up by node.
#[derive(Default)]
pub struct Labels {
    by_node: HashMap<usize, Label>,
}

impl Labels {
    pub fn new(findings: &[Finding]) -> Self {
        let mut by_node = HashMap::new();
        // Unpacked Git data is fixed by `git gc`, not by deleting: its `.git` folders must
        // never go, so they're left uncoloured rather than shown as something to delete.
        for finding in findings.iter().filter(|f| f.fix == Fix::Trash) {
            let label = Label { safety: finding.safety, title: finding.title };
            for &ix in &finding.nodes {
                // Were a folder in two findings, the more careful one wins.
                by_node
                    .entry(ix)
                    .and_modify(|old: &mut Label| {
                        if label.safety == Safety::Review {
                            *old = label;
                        }
                    })
                    .or_insert(label);
            }
        }
        Self { by_node }
    }

    /// What the nearest finding at or above `ix` says about it. Everything inside a finding's
    /// folder is covered by it, and the nearest finding wins: a project's `node_modules` in
    /// Downloads is safe to delete, though Downloads needs a look.
    pub fn of(&self, tree: &Tree, mut ix: usize) -> Option<Label> {
        if self.by_node.is_empty() {
            return None;
        }
        loop {
            if let Some(&label) = self.by_node.get(&ix) {
                return Some(label);
            }
            ix = tree.nodes[ix].parent?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Kind, Node};
    use std::path::PathBuf;

    // root ── Downloads ── setup.dmg
    //      │            └─ project ── node_modules ── lib.js
    //      ├─ Library ── Caches ── app ── blob
    //      ├─ Documents ── essay.txt
    //      └─ code ── .git
    fn tree() -> Tree {
        let node = |name: &str, kind, parent, children| Node { name: name.into(), size: 1, kind, parent, children, items: 0 };
        let nodes = vec![
            node("root", Kind::Dir, None, vec![1, 6, 10, 12]),
            node("Downloads", Kind::Dir, Some(0), vec![2, 3]),
            node("setup.dmg", Kind::File, Some(1), vec![]),
            node("project", Kind::Dir, Some(1), vec![4]),
            node("node_modules", Kind::Dir, Some(3), vec![5]),
            node("lib.js", Kind::File, Some(4), vec![]),
            node("Library", Kind::Dir, Some(0), vec![7]),
            node("Caches", Kind::Dir, Some(6), vec![8]),
            node("app", Kind::Dir, Some(7), vec![9]),
            node("blob", Kind::File, Some(8), vec![]),
            node("Documents", Kind::Dir, Some(0), vec![11]),
            node("essay.txt", Kind::File, Some(10), vec![]),
            node("code", Kind::Dir, Some(0), vec![13]),
            node(".git", Kind::Dir, Some(12), vec![]),
        ];
        Tree { root_path: PathBuf::from("/"), nodes, errors: 0, cloud_only: 0, ..Default::default() }
    }

    fn finding(title: &'static str, safety: Safety, nodes: Vec<usize>, fix: Fix) -> Finding {
        Finding { title, blurb: String::new(), safety, size: 1, allocated: 1, path: None, nodes, pending: false, fix }
    }

    fn findings() -> Vec<Finding> {
        vec![
            finding("Downloads", Safety::Review, vec![1], Fix::Trash),
            finding("node_modules", Safety::Safe, vec![4], Fix::Trash),
            finding("App caches", Safety::Safe, vec![7], Fix::Trash),
            finding("Unpacked Git data", Safety::Review, vec![13], Fix::GitGc),
        ]
    }

    #[test]
    fn a_finding_colours_its_folder() {
        let (tree, labels) = (tree(), Labels::new(&findings()));
        assert_eq!(labels.of(&tree, 7), Some(Label { safety: Safety::Safe, title: "App caches" }));
        assert_eq!(labels.of(&tree, 1).map(|l| l.safety), Some(Safety::Review));
        assert_eq!(color(labels.of(&tree, 7).map(|l| l.safety)), SAFE);
        assert_eq!(color(labels.of(&tree, 1).map(|l| l.safety)), REVIEW);
    }

    #[test]
    fn everything_inside_a_finding_takes_its_colour_and_the_nearest_wins() {
        let (tree, labels) = (tree(), Labels::new(&findings()));
        assert_eq!(labels.of(&tree, 9).map(|l| l.title), Some("App caches"), "a file deep inside");
        assert_eq!(labels.of(&tree, 8).map(|l| l.title), Some("App caches"));
        assert_eq!(labels.of(&tree, 2).map(|l| l.title), Some("Downloads"));
        assert_eq!(labels.of(&tree, 3).map(|l| l.title), Some("Downloads"), "a folder in Downloads that isn't a finding itself");
        // node_modules inside Downloads: its own finding is nearer.
        assert_eq!(labels.of(&tree, 4).map(|l| l.safety), Some(Safety::Safe));
        assert_eq!(labels.of(&tree, 5).map(|l| l.title), Some("node_modules"));
    }

    #[test]
    fn what_no_finding_covers_is_neutral() {
        let (tree, labels) = (tree(), Labels::new(&findings()));
        // Not in any finding.
        assert_eq!(labels.of(&tree, 10), None);
        assert_eq!(labels.of(&tree, 11), None);
        // Holding findings doesn't make a folder one: Library and the root stay neutral.
        assert_eq!(labels.of(&tree, 6), None);
        assert_eq!(labels.of(&tree, 0), None);
        // Git data is packed with `git gc`, not deleted, so `.git` isn't coloured.
        assert_eq!(labels.of(&tree, 13), None);
        assert_eq!(color(None), NEUTRAL);
        assert!(NEUTRAL != SAFE && NEUTRAL != REVIEW);
        // With no findings at all, nothing is coloured.
        assert!((0..tree.nodes.len()).all(|ix| Labels::new(&[]).of(&tree, ix).is_none()));
    }

    #[test]
    fn a_folder_in_two_findings_gets_the_more_careful_label() {
        let tree = tree();
        let both = [finding("Safe one", Safety::Safe, vec![10], Fix::Trash), finding("Careful one", Safety::Review, vec![10], Fix::Trash)];
        assert_eq!(Labels::new(&both).of(&tree, 11).map(|l| l.safety), Some(Safety::Review));
        let reversed = [both[1].clone(), both[0].clone()];
        assert_eq!(Labels::new(&reversed).of(&tree, 11).map(|l| l.safety), Some(Safety::Review));
    }

    #[test]
    fn the_legend_explains_every_colour() {
        for safety in [Some(Safety::Safe), Some(Safety::Review), None] {
            assert!(LEGEND.iter().any(|&(s, _)| s == safety), "{safety:?} is in the legend");
        }
        for (safety, text) in LEGEND {
            if let Some(safety) = safety {
                assert_eq!(text, label(safety));
            }
        }
    }
}
