// SPDX-License-Identifier: AGPL-3.0-or-later

use gitcko_domain::RepoPath;
use proptest::prelude::*;

#[test]
fn every_windows_fixture_case_gets_its_verdict() {
    let cases = gitcko_test_support::windows_path_cases();
    assert!(cases.len() >= 30, "fixture file looks truncated");
    let mut wrong = Vec::new();
    for case in &cases {
        let parsed = RepoPath::parse(&case.path);
        if parsed.is_ok() != case.valid {
            wrong.push(format!(
                "{:?}: valid should be {} ({})",
                case.path, case.valid, case.why
            ));
        }
        let windows_safe = parsed.is_ok_and(|p| p.windows_problem().is_none());
        if windows_safe != case.windows_safe {
            wrong.push(format!(
                "{:?}: windows_safe should be {} ({})",
                case.path, case.windows_safe, case.why
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn error_names_the_path_and_the_problem() {
    let err = RepoPath::parse("a/../../etc").unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("a/../../etc") && message.contains(".."),
        "{message}"
    );
}

proptest! {
    // Whatever string comes in (a tree entry, a model's claimed path, a config value),
    // an accepted path can never point outside the repository.
    #[test]
    fn accepted_paths_never_escape(input in "(\\.{1,3}|[a-z]{1,3}|/|\\\\|:|~|\\.git){0,8}") {
        if let Ok(path) = RepoPath::parse(&input) {
            let s = path.as_str();
            prop_assert_eq!(s, input.as_str());
            prop_assert!(!s.starts_with('/'));
            prop_assert!(s.split('/').all(|c| !c.is_empty() && c != "." && c != ".."));
            prop_assert!(s.split('/').all(|c| !c.eq_ignore_ascii_case(".git")));
        }
    }

    #[test]
    fn ordinary_paths_are_accepted(parts in proptest::collection::vec("[a-zA-Z0-9_-][a-zA-Z0-9_.-]{0,10}", 1..6)) {
        let joined = parts.join("/");
        prop_assume!(parts.iter().all(|p| !p.to_ascii_lowercase().starts_with(".git")));
        prop_assert!(RepoPath::parse(&joined).is_ok(), "{}", joined);
    }
}
