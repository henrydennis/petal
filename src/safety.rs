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
/// Findings that belong to an app ("Manage in app"): not for the Trash.
pub const MANAGE: u32 = 0xa371f7;
/// A plain grey, not a muted green: nothing is known about it.
pub const NEUTRAL: u32 = 0x5c5f66;

/// The colour for a finding's verdict, or for what no finding covers (`None`). A new kind of
/// verdict needs a colour here and a line in `LEGEND`; the compiler points to this match.
pub fn color(safety: Option<Safety>) -> u32 {
    match safety {
        Some(Safety::Safe) => SAFE,
        Some(Safety::Review) => REVIEW,
        Some(Safety::ManageInApp) => MANAGE,
        None => NEUTRAL,
    }
}

/// How the findings list and the chart's legend name a verdict.
pub fn label(safety: Safety) -> &'static str {
    match safety {
        Safety::Safe => "Safe to delete",
        Safety::Review => "Review first",
        Safety::ManageInApp => "Manage in app",
    }
}

/// How careful a verdict asks you to be, for when two findings cover the same folder:
/// leaving something to its app is more careful than looking first, which is more careful
/// than deleting it.
fn care(safety: Safety) -> u8 {
    match safety {
        Safety::Safe => 0,
        Safety::Review => 1,
        Safety::ManageInApp => 2,
    }
}

/// What the chart's legend explains, in order.
pub const LEGEND: [(Option<Safety>, &str); 4] = [
    (Some(Safety::Safe), "Safe to delete"),
    (Some(Safety::Review), "Review first"),
    (Some(Safety::ManageInApp), "Manage in app"),
    (None, "No finding"),
];

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
        // Findings cleared with a command (Homebrew, pnpm, Docker) or in their app are
        // coloured by their verdict like any other.
        for finding in findings.iter().filter(|f| f.fix != Fix::GitGc) {
            let label = Label { safety: finding.safety, title: finding.title };
            for &ix in &finding.nodes {
                // Were a folder in two findings, the more careful one wins.
                by_node
                    .entry(ix)
                    .and_modify(|old: &mut Label| {
                        if care(label.safety) > care(old.safety) {
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
    //      │          │         └─ Homebrew
    //      │          └─ Docker.raw
    //      ├─ Documents ── essay.txt
    //      ├─ code ── .git
    //      └─ .claude ── projects ── chat.jsonl
    fn tree() -> Tree {
        let node = |name: &str, kind, parent, children| Node { name: name.into(), size: 1, kind, parent, children, items: 0 };
        let nodes = vec![
            node("root", Kind::Dir, None, vec![1, 6, 10, 12, 14]),
            node("Downloads", Kind::Dir, Some(0), vec![2, 3]),
            node("setup.dmg", Kind::File, Some(1), vec![]),
            node("project", Kind::Dir, Some(1), vec![4]),
            node("node_modules", Kind::Dir, Some(3), vec![5]),
            node("lib.js", Kind::File, Some(4), vec![]),
            node("Library", Kind::Dir, Some(0), vec![7, 18]),
            node("Caches", Kind::Dir, Some(6), vec![8, 17]),
            node("app", Kind::Dir, Some(7), vec![9]),
            node("blob", Kind::File, Some(8), vec![]),
            node("Documents", Kind::Dir, Some(0), vec![11]),
            node("essay.txt", Kind::File, Some(10), vec![]),
            node("code", Kind::Dir, Some(0), vec![13]),
            node(".git", Kind::Dir, Some(12), vec![]),
            node(".claude", Kind::Dir, Some(0), vec![15]),
            node("projects", Kind::Dir, Some(14), vec![16]),
            node("chat.jsonl", Kind::File, Some(15), vec![]),
            node("Homebrew", Kind::Dir, Some(7), vec![]),
            node("Docker.raw", Kind::File, Some(6), vec![]),
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
            finding("Claude Code history", Safety::ManageInApp, vec![15], Fix::InApp),
            finding("Homebrew downloads", Safety::Safe, vec![17], Fix::Command("brew cleanup --prune=all")),
            finding("Docker disk image", Safety::ManageInApp, vec![18], Fix::Command("docker system prune")),
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
        assert!(NEUTRAL != SAFE && NEUTRAL != REVIEW && NEUTRAL != MANAGE);
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
    fn manage_in_app_is_the_most_careful_label() {
        let tree = tree();
        let safe = finding("Safe one", Safety::Safe, vec![10], Fix::Trash);
        let review = finding("Review one", Safety::Review, vec![10], Fix::Trash);
        let manage = finding("App's own", Safety::ManageInApp, vec![10], Fix::InApp);
        let orders = [
            [safe.clone(), review.clone(), manage.clone()],
            [manage.clone(), review.clone(), safe.clone()],
            [review.clone(), manage.clone(), safe.clone()],
            [safe.clone(), manage.clone(), review.clone()],
        ];
        for order in orders {
            assert_eq!(Labels::new(&order).of(&tree, 11).map(|l| l.title), Some("App's own"));
        }
        // A safe label never displaces a more careful one, whichever comes first.
        assert_eq!(Labels::new(&[review.clone(), safe.clone()]).of(&tree, 10).map(|l| l.title), Some("Review one"));
        assert_eq!(Labels::new(&[manage.clone(), review]).of(&tree, 10).map(|l| l.title), Some("App's own"));
        assert_eq!(Labels::new(&[manage, safe]).of(&tree, 10).map(|l| l.title), Some("App's own"));
    }

    #[test]
    fn findings_cleared_another_way_take_their_verdicts_colour() {
        let (tree, labels) = (tree(), Labels::new(&findings()));
        // Managed in its app: the folder and everything in it are purple.
        assert_eq!(labels.of(&tree, 15).map(|l| l.title), Some("Claude Code history"));
        assert_eq!(color(labels.of(&tree, 16).map(|l| l.safety)), MANAGE);
        assert_eq!(labels.of(&tree, 14), None, "the folder holding it isn't covered");
        // Cleared with a command: coloured by the verdict, a file as much as a folder.
        assert_eq!(color(labels.of(&tree, 18).map(|l| l.safety)), MANAGE);
        assert_eq!(labels.of(&tree, 18).map(|l| l.title), Some("Docker disk image"));
        // Homebrew's downloads inside App caches: its own finding is nearer.
        assert_eq!(labels.of(&tree, 17), Some(Label { safety: Safety::Safe, title: "Homebrew downloads" }));
        assert_eq!(color(labels.of(&tree, 17).map(|l| l.safety)), SAFE);
        // Library holds Docker's disk image but is still not a finding.
        assert_eq!(labels.of(&tree, 6), None);
    }

    #[test]
    fn the_legend_explains_every_colour() {
        for safety in [Some(Safety::Safe), Some(Safety::Review), Some(Safety::ManageInApp), None] {
            assert!(LEGEND.iter().any(|&(s, _)| s == safety), "{safety:?} is in the legend");
        }
        let colors: std::collections::HashSet<u32> = LEGEND.iter().map(|&(s, _)| color(s)).collect();
        assert_eq!(colors.len(), LEGEND.len(), "every line of the legend has its own colour");
        for (safety, text) in LEGEND {
            if let Some(safety) = safety {
                assert_eq!(text, label(safety));
            }
        }
    }
}
