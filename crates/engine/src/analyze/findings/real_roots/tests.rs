use unlace_ir::{ContainerTree, Layout, build_laminar_tree};

use super::RealRoots;
use crate::result::{ContainerNode, Level};

/// The laminar tree of `files` under the package group `root`.
fn tree_of(files: &[&str], root: &str, package_roots: &[&str]) -> ContainerTree {
    let paths: Vec<_> = files.iter().map(|file| (*file).into()).collect();
    let layout = Layout {
        package_roots: package_roots.iter().map(|path| (*path).into()).collect(),
        source_roots: vec!["src".into()],
    };
    build_laminar_tree(&paths, root, &layout).tree
}

/// A rendered container holding one `file` beneath it.
fn node_over(name: &str, level: Level, child: ContainerNode) -> ContainerNode {
    ContainerNode {
        name: name.to_owned(),
        level,
        children: Some(vec![child]),
        symbols: None,
        production_sloc: None,
    }
}

/// The domain of one `file` and the package holding that domain.
fn domain_and_package(package: &str, domain: &str, file: &str) -> (ContainerNode, ContainerNode) {
    let file = ContainerNode {
        name: file.to_owned(),
        level: Level::File,
        children: None,
        symbols: None,
        production_sloc: None,
    };
    let folder = node_over(domain, Level::Folder, file);
    let domain = node_over(domain, Level::Domain, folder);
    let package = node_over(package, Level::Package, domain.clone());
    (domain, package)
}

#[test]
fn should_keep_the_leading_directory_of_a_package_that_shares_the_root_name() {
    // `ws/atlas` is the real package directory below the repository root `ws`
    // when the root holds a directory of its own name: its first segment equals
    // the root's name without being the root's label.
    let file = "ws/atlas/src/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[file], "ws", &["ws/atlas"]));
    let (_, package) = domain_and_package("ws/atlas", "ws/atlas/agent", file);

    assert_eq!(roots.paths(&package, Level::Package), ["ws/atlas"]);
}

#[test]
fn should_keep_the_leading_directory_of_a_domain_that_shares_the_root_name() {
    let file = "ws/atlas/src/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[file], "ws", &["ws/atlas"]));
    let (domain, _) = domain_and_package("ws/atlas", "ws/atlas/agent", file);

    assert_eq!(
        roots.paths(&domain, Level::Domain),
        ["ws/atlas/src/agent"],
        "the see-through `src` root is restored below the real package"
    );
}

#[test]
fn should_drop_the_root_label_from_a_package_keyed_under_it() {
    // a repository without manifest members keys its one package, and every
    // domain below it, under the root's label: `ws/agent` stands for `src/agent`.
    let file = "src/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[file], "ws", &[]));
    let (domain, package) = domain_and_package("ws", "ws/agent", file);

    assert_eq!(roots.paths(&package, Level::Package), ["."]);
    assert_eq!(roots.paths(&domain, Level::Domain), ["src/agent"]);
}

#[test]
fn should_drop_one_root_label_and_keep_a_real_directory_that_repeats_it() {
    // `ws/ws/agent` below the label is the real directory `ws/agent`.
    let file = "ws/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[file], "ws", &[]));
    let (domain, _) = domain_and_package("ws", "ws/ws", file);

    assert_eq!(roots.paths(&domain, Level::Domain), ["ws"]);
}

#[test]
fn should_name_a_manifest_package_whose_directory_is_named_like_the_root() {
    // `tokio/tokio`: the manifest sits in a directory of the root's own name, so
    // the package key `ws` is a real directory and not the group label.
    let file = "ws/src/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[file], "ws", &["ws"]));
    let (domain, package) = domain_and_package("ws", "ws/agent", file);

    assert_eq!(roots.paths(&package, Level::Package), ["ws"]);
    assert_eq!(roots.paths(&domain, Level::Domain), ["ws/src/agent"]);
}

#[test]
fn should_drop_the_label_from_a_loose_directory_named_like_a_real_package() {
    // the loose `atlas/foo` is keyed `ws/atlas` under the label `ws`, the very
    // key of the real package `ws/atlas` beside it: provenance, not the string,
    // decides which one is real.
    let loose = "atlas/foo/x.ts";
    let real = "ws/atlas/src/agent/loop.ts";
    let roots = RealRoots::from_tree(&tree_of(&[loose, real], "ws", &["ws/atlas"]));
    let (loose_domain, _) = domain_and_package("ws", "ws/atlas", loose);
    let (real_domain, real_package) = domain_and_package("ws/atlas", "ws/atlas/agent", real);

    assert_eq!(roots.paths(&loose_domain, Level::Domain), ["atlas"]);
    assert_eq!(
        roots.paths(&real_domain, Level::Domain),
        ["ws/atlas/src/agent"]
    );
    assert_eq!(roots.paths(&real_package, Level::Package), ["ws/atlas"]);
}

#[test]
fn should_keep_loose_files_and_a_same_named_package_apart() {
    // loose files and a manifest package named like the root intern one package
    // container; the domains stay distinct and each keeps its own provenance.
    let real = "ws/src/agent/loop.ts";
    let loose = "loose/y.ts";
    let roots = RealRoots::from_tree(&tree_of(&[real, loose], "ws", &["ws"]));
    let (real_domain, _) = domain_and_package("ws", "ws/agent", real);
    let (loose_domain, _) = domain_and_package("ws", "ws/loose", loose);

    assert_eq!(roots.paths(&real_domain, Level::Domain), ["ws/src/agent"]);
    assert_eq!(roots.paths(&loose_domain, Level::Domain), ["loose"]);
}
