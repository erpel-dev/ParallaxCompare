//! The result tree as a session keeps it, and the part of it a view is sent.
//!
//! A comparison of large trees is mostly identical files. The session keeps the
//! whole tree; a view gets everything that differs, and identical folders
//! without their entries, which it asks for when the folder is opened.

use serde::Serialize;

use crate::compare::{Node, Side, Status, folder_status};
use crate::vfs::EntryKind;

/// A node as a view shows it. An identical folder is sent without its entries
/// (`unexpanded`, with the number of files in it) unless they are asked for.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ViewNode {
    pub name: String,
    pub left: Option<Side>,
    pub right: Option<Side>,
    pub status: Status,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ViewNode>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unexpanded: bool,
    /// For an identical folder sent without its entries: the files in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Whether a view gets `node` without its entries: a folder on both sides whose
/// entries are all the same. Archives keep theirs: closed, they count as a file.
fn withheld(node: &Node) -> bool {
    let dir = |side: Option<Side>| side.is_some_and(|s| s.kind == EntryKind::Dir);
    node.status == Status::Same
        && !node.unexpanded()
        && !node.children().is_empty()
        && dir(node.left())
        && dir(node.right())
}

/// The files below `node` as a view counts them: a closed archive is one file,
/// and a folder that was not walked has none that are known.
pub fn files_below(node: &Node) -> u64 {
    node.children()
        .iter()
        .map(|c| {
            let closed_archive = c.unexpanded() && c.left().or(c.right()).is_some_and(|s| s.kind == EntryKind::Archive);
            if c.is_container() && c.status != Status::Error && !closed_archive { files_below(c) } else { 1 }
        })
        .sum()
}

/// `node` for a view, with its entries. With `deep`, everything below it;
/// otherwise identical folders below it come without their entries.
pub fn view(node: &Node, deep: bool) -> ViewNode {
    let mut out = shallow(node);
    out.children = node.children().iter().map(|c| if deep { view(c, true) } else { entry(c) }).collect();
    out
}

/// `node` as the view of its folder holds it: an identical folder without its
/// entries, anything else with them.
pub fn entry(node: &Node) -> ViewNode {
    if withheld(node) {
        ViewNode { unexpanded: true, same_files: Some(files_below(node)), ..shallow(node) }
    } else {
        view(node, false)
    }
}

/// The path to compare again so that a change at `path` shows in the tree: the
/// path itself when the folder holding it was walked, a new entry as a whole,
/// otherwise the deepest folder that was.
pub fn refresh_target(root: &Node, path: &str) -> String {
    let mut node = root;
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        if node.unexpanded() || node.children().is_empty() {
            return at;
        }
        let here = crate::vfs::join(&at, part);
        match node.children().iter().find(|c| c.name() == part) {
            Some(child) => node = child,
            None => return here,
        }
        at = here;
    }
    path.to_string()
}

fn shallow(node: &Node) -> ViewNode {
    ViewNode {
        name: node.name().to_string(),
        left: node.left(),
        right: node.right(),
        status: node.status,
        children: Vec::new(),
        unexpanded: node.unexpanded(),
        same_files: None,
        message: node.message().map(str::to_string),
    }
}

/// The node at `path` below `root`; `root` itself for the empty path.
pub fn find<'a>(root: &'a Node, path: &str) -> Option<&'a Node> {
    path.split('/')
        .filter(|p| !p.is_empty())
        .try_fold(root, |node, part| node.children().iter().find(|c| c.name() == part))
}

/// Paths of what was not walked at or below `path` (folders identical by their
/// Git tree id, closed archives), for comparing them when all entries are wanted.
pub fn unwalked(root: &Node, path: &str) -> Vec<String> {
    fn walk(node: &Node, path: &str, out: &mut Vec<String>) {
        if node.unexpanded() {
            out.push(path.to_string());
        }
        for child in node.children() {
            walk(child, &crate::vfs::join(path, child.name()), out);
        }
    }
    let mut out = Vec::new();
    if let Some(node) = find(root, path) {
        walk(node, path, &mut out);
    }
    out
}

/// Puts `replacement` where `path` is (or removes the node there, for `None`)
/// and brings the status of the folders above it up to date. A new node goes
/// where the compare engine would have put it: folders first, then by name.
/// Returns whether the tree had a place for it; the root is replaced as a whole.
pub fn replace(root: &mut Node, path: &str, replacement: Option<Node>) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let Some((name, parents)) = parts.split_last() else {
        return match replacement {
            Some(node) => {
                *root = node;
                true
            }
            None => false,
        };
    };
    fn place(node: &mut Node, parents: &[&str], name: &str, replacement: Option<Node>, is_root: bool) -> bool {
        let done = match parents.split_first() {
            Some((next, rest)) => match node.children_mut().iter_mut().find(|c| c.name() == *next) {
                Some(child) => place(child, rest, name, replacement, false),
                None => false,
            },
            None => {
                let children = node.children_mut();
                let at = children.iter().position(|c| c.name() == name);
                match (replacement, at) {
                    (Some(mut new), at) => {
                        new.set_name(name);
                        match at {
                            Some(at) => children[at] = new,
                            None => {
                                let key = |n: &Node| (!n.is_container(), n.name().to_lowercase(), n.name().to_string());
                                let k = key(&new);
                                let at = children.iter().position(|c| key(c) > k).unwrap_or(children.len());
                                children.insert(at, new);
                            }
                        }
                    }
                    (None, Some(at)) => {
                        children.remove(at);
                    }
                    (None, None) => {}
                }
                true
            }
        };
        // As the engine sets it: a folder on both sides takes its status from its entries.
        let holds = |side: Option<Side>| side.is_some_and(|s| s.kind.is_container());
        if done && !is_root && holds(node.left()) && holds(node.right()) {
            node.status = folder_status(node.children());
        }
        done
    }
    place(root, parents, name, replacement, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(kind: EntryKind) -> Option<Side> {
        Some(Side { kind, size: 1, mtime: Some(5), ignored: false })
    }

    fn file(name: &str, status: Status) -> Node {
        let (l, r) = match status {
            Status::LeftOnly => (side(EntryKind::File), None),
            Status::RightOnly => (None, side(EntryKind::File)),
            _ => (side(EntryKind::File), side(EntryKind::File)),
        };
        let mut node = Node::new(name, l, r);
        node.status = status;
        node
    }

    fn dir(name: &str, children: Vec<Node>) -> Node {
        let mut node = Node::new(name, side(EntryKind::Dir), side(EntryKind::Dir));
        node.status = folder_status(&children);
        node.set_children(children);
        node
    }

    fn sample() -> Node {
        dir(
            "",
            vec![
                dir("equal", vec![dir("deeper", vec![file("x", Status::Same)]), file("a", Status::Same)]),
                dir("mixed", vec![dir("quiet", vec![file("q", Status::Same)]), file("b", Status::Different)]),
                file("top", Status::Same),
            ],
        )
    }

    #[test]
    fn nodes_are_compact() {
        assert!(std::mem::size_of::<Node>() <= 72, "{}", std::mem::size_of::<Node>());
        let node = Node::new("n", side(EntryKind::Symlink), None);
        assert_eq!(node.left(), side(EntryKind::Symlink));
        assert_eq!(node.right(), None);
        let ignored = Some(Side { kind: EntryKind::Archive, size: u64::MAX, mtime: None, ignored: true });
        let mut node = Node::new("a-name-longer-than-what-is-stored-inline.txt", None, ignored);
        assert_eq!((node.left(), node.right()), (None, ignored));
        assert_eq!(node.name(), "a-name-longer-than-what-is-stored-inline.txt");
        node.set_unexpanded(true);
        node.set_message(Some("why"));
        assert!(node.unexpanded() && node.message() == Some("why") && node.right() == ignored);
    }

    #[test]
    fn a_view_gets_identical_folders_without_their_entries() {
        let root = sample();
        let v = view(&root, false);
        let names = |n: &ViewNode| n.children.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&v), ["equal", "mixed", "top"]);
        let equal = &v.children[0];
        assert!(equal.unexpanded && equal.children.is_empty());
        assert_eq!(equal.same_files, Some(2));
        // What differs is all there; an identical folder inside it is withheld again.
        let mixed = &v.children[1];
        assert!(!mixed.unexpanded && names(mixed) == ["quiet", "b"]);
        assert!(mixed.children[0].unexpanded && mixed.children[0].same_files == Some(1));
        // Asked for, a folder comes with its entries, one level at a time.
        let opened = view(find(&root, "equal").unwrap(), false);
        assert_eq!(names(&opened), ["deeper", "a"]);
        assert!(opened.children[0].unexpanded && !opened.unexpanded);
        // Deep: everything.
        let all = view(&root, true);
        assert_eq!(all.children[0].children[0].children[0].name, "x");
        assert!(!all.children[0].unexpanded && all.children[0].same_files.is_none());
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["children"][0]["sameFiles"], 2);
        assert_eq!(json["children"][0]["unexpanded"], true);
        assert!(json["children"][2].get("unexpanded").is_none());
    }

    #[test]
    fn replacing_keeps_order_and_status() {
        let mut root = sample();
        assert_eq!(find(&root, "equal").unwrap().status, Status::Same);
        // A changed file makes the folders above it different.
        assert!(replace(&mut root, "equal/deeper/x", Some(file("x", Status::Different))));
        assert_eq!(find(&root, "equal/deeper").unwrap().status, Status::Different);
        assert_eq!(find(&root, "equal").unwrap().status, Status::Different);
        // New entries go folders first, then by name.
        assert!(replace(&mut root, "mixed/a-new", Some(file("ignored-name", Status::LeftOnly))));
        assert!(replace(&mut root, "mixed/zz", Some(dir("zz", vec![]))));
        let names: Vec<_> = find(&root, "mixed").unwrap().children().iter().map(|c| c.name().to_string()).collect();
        assert_eq!(names, ["quiet", "zz", "a-new", "b"]);
        // Removing the only differences makes the folder the same again.
        assert!(replace(&mut root, "mixed/b", None));
        assert!(replace(&mut root, "mixed/a-new", None));
        assert_eq!(find(&root, "mixed").unwrap().status, Status::Same);
        // No place for it: a missing parent.
        assert!(!replace(&mut root, "nowhere/x", Some(file("x", Status::Same))));
        assert_eq!(unwalked(&root, ""), Vec::<String>::new());
        let mut skipped = dir("skipped", vec![]);
        skipped.set_unexpanded(true);
        assert!(replace(&mut root, "mixed/skipped", Some(skipped)));
        assert_eq!(unwalked(&root, ""), ["mixed/skipped"]);
        assert_eq!(files_below(&root), 4);
        // A change is compared where the tree has it, or at the folder that was walked.
        assert_eq!(refresh_target(&root, "equal/deeper/x"), "equal/deeper/x");
        assert_eq!(refresh_target(&root, "equal/deeper/new/file"), "equal/deeper/new");
        assert_eq!(refresh_target(&root, "mixed/skipped/a/b"), "mixed/skipped");
        assert_eq!(refresh_target(&root, "top/below-a-file"), "top");
        // On its own, an identical folder comes without its entries.
        let quiet = entry(find(&root, "mixed/quiet").unwrap());
        assert!(quiet.unexpanded && quiet.children.is_empty() && quiet.same_files == Some(1));
        assert!(!entry(find(&root, "equal").unwrap()).unexpanded);
    }
}
