//! S5: two distinct (tenant, repo) pairs never share a store path or a route, whatever the names.

use std::path::{Component, Path};

use proptest::prelude::*;
use tessel_server::id::RepoId;
use tessel_server::ws::{parse_path, Target};

/// Mostly valid names from a small alphabet, so equal and near-equal pairs are common, and
/// sometimes names built from the characters that make joins collide or paths escape.
fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[ab0]{1,2}(-[ab0]{1,2}){0,2}",
        1 => "[a-zA-Z0-9./ \\\\%-]{0,8}",
        1 => prop_oneof![Just(".".to_string()), Just("..".to_string()), Just("a/b".to_string())],
    ]
}

proptest! {
    #[test]
    fn distinct_pairs_never_share_a_store_path_or_a_route(
        first in (name(), name()),
        second in (name(), name()),
    ) {
        let root = Path::new("/stores");
        let (Ok(a), Ok(b)) = (
            RepoId::new(&first.0, &first.1),
            RepoId::new(&second.0, &second.1),
        ) else {
            return Ok(());
        };
        if a != b {
            prop_assert_ne!(a.store_file(root), b.store_file(root));
            prop_assert_ne!(a.route_base(), b.route_base());
        }
    }

    /// The pairs that collide when joined by the hyphen a name may hold: the same three parts
    /// split at either hyphen.
    #[test]
    fn the_same_parts_split_differently_stay_apart(
        x in "[ab0]{1,2}",
        y in "[ab0]{1,2}",
        z in "[ab0]{1,2}",
    ) {
        let root = Path::new("/stores");
        let (Ok(a), Ok(b)) = (
            RepoId::new(&format!("{x}-{y}"), &z),
            RepoId::new(&x, &format!("{y}-{z}")),
        ) else {
            return Err(TestCaseError::fail("both pairs are valid names"));
        };
        prop_assert_ne!(a.store_file(root), b.store_file(root));
        prop_assert_ne!(a.route_base(), b.route_base());
    }

    #[test]
    fn a_valid_pair_stays_inside_the_store_directory_and_its_route_names_it_back(
        tenant in name(),
        repo in name(),
    ) {
        let Ok(id) = RepoId::new(&tenant, &repo) else {
            return Ok(());
        };
        let root = Path::new("/stores");
        let file = id.store_file(root);
        let relative = file.strip_prefix(root).unwrap_or(&file);
        let parts: Vec<Component<'_>> = relative.components().collect();
        prop_assert_eq!(parts.len(), 2, "a tenant directory and a file: {:?}", file);
        prop_assert!(parts.iter().all(|part| matches!(part, Component::Normal(_))));
        for (tail, target) in [("ws", Target::Ws), ("summary", Target::Summary)] {
            let path = format!("{}/{tail}", id.route_base());
            prop_assert_eq!(parse_path(&path), Some((id.clone(), target)));
        }
    }

    #[test]
    fn a_name_with_a_separator_a_dot_or_a_capital_names_nothing(
        tenant in name(),
        repo in name(),
    ) {
        let outside = |text: &str| {
            text.chars()
                .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        };
        if outside(&tenant) || outside(&repo) {
            prop_assert!(RepoId::new(&tenant, &repo).is_err());
        }
    }
}

#[test]
fn the_names_that_collide_when_joined_stay_apart() {
    let root = Path::new("/stores");
    let pairs = [
        ("a-b", "c"),
        ("a", "b-c"),
        ("a", "b"),
        ("ab", "c"),
        ("a-b-c", "d"),
    ];
    let ids: Vec<RepoId> = pairs
        .iter()
        .filter_map(|(tenant, repo)| RepoId::new(tenant, repo).ok())
        .collect();
    assert_eq!(ids.len(), pairs.len());
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert_ne!(a.store_file(root), b.store_file(root), "{a} and {b}");
            assert_ne!(a.route_base(), b.route_base(), "{a} and {b}");
        }
    }
    for hostile in [
        (".", "x"),
        ("..", "x"),
        ("a", ".."),
        ("a/b", "c"),
        ("a", "b/c"),
        ("a", "../b"),
    ] {
        assert!(RepoId::new(hostile.0, hostile.1).is_err(), "{hostile:?}");
    }
}
