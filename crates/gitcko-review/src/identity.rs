// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stable ids (REVIEW_ENGINE §16): a hash of category, logical entity and the cited
//! code. Never line numbers, which move, and never the model's prose, which it rewords.

use std::collections::BTreeMap;

use gitcko_domain::ContentHash;
use gitcko_prompts::Category;

/// Code with its whitespace collapsed, so reformatting doesn't change an issue.
pub fn normalise(code: &str) -> String {
    code.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A title's words, lowercased and sorted: two reports of one problem that word it
/// differently still compare equal. Used to merge reports and to order same-code issues,
/// never to name one.
pub fn title_signature(title: &str) -> String {
    let mut words: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    words.join(" ")
}

fn hashed(prefix: &str, category: Category, entity: &str, code: &str, nth: usize) -> String {
    // NUL can't occur in a path or name, so the parts can't run into each other.
    let text = format!("{category:?}\0{entity}\0{code}\0{nth}");
    let hash = ContentHash::of(text.as_bytes()).to_string();
    format!("{prefix}{}", &hash[..16])
}

/// One id per issue: `(category, entity, code, title)`. Independent bugs on the same
/// lines share the first three, and must not share an id, so after the first of them (in
/// title order) a number is mixed in. The result is in the order of `issues`.
pub fn assign(issues: &[(Category, &str, &str, &str)]) -> Vec<String> {
    let mut groups: BTreeMap<(Category, &str, &str), Vec<usize>> = BTreeMap::new();
    for (at, (category, entity, code, _)) in issues.iter().enumerate() {
        groups
            .entry((*category, entity, code))
            .or_default()
            .push(at);
    }
    let mut ids = vec![String::new(); issues.len()];
    for ((category, entity, code), mut members) in groups {
        members.sort_by_key(|at| (title_signature(issues[*at].3), *at));
        for (nth, at) in members.into_iter().enumerate() {
            ids[at] = hashed("f_", category, entity, code, nth);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn finding_id(category: Category, entity: &str, code: &str) -> String {
        hashed("f_", category, entity, code, 0)
    }

    #[test]
    fn an_id_is_f_and_sixteen_hex_characters() {
        let id = finding_id(Category::Security, "fn a", "x / y");
        assert_eq!(id.len(), 18, "{id}");
        assert!(id.starts_with("f_"));
        assert!(id[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn category_entity_and_code_each_change_the_id() {
        let base = finding_id(Category::Correctness, "a::f", "x / y");
        assert_eq!(base, finding_id(Category::Correctness, "a::f", "x / y"));
        assert_ne!(base, finding_id(Category::Security, "a::f", "x / y"));
        assert_ne!(base, finding_id(Category::Correctness, "a::g", "x / y"));
        assert_ne!(base, finding_id(Category::Correctness, "a::f", "x / z"));
    }

    #[test]
    fn the_parts_cannot_run_into_each_other() {
        assert_ne!(
            finding_id(Category::Correctness, "ab", "c"),
            finding_id(Category::Correctness, "a", "bc")
        );
    }

    #[test]
    fn issues_on_the_same_lines_get_different_ids_whatever_the_order() {
        let a = (Category::Correctness, "f", "x / y", "divides by zero");
        let b = (Category::Correctness, "f", "x / y", "overflows");
        let c = (Category::Security, "f", "x / y", "overflows");
        let forward = assign(&[a, b, c]);
        let backward = assign(&[c, b, a]);
        assert_eq!(forward[0], backward[2]);
        assert_eq!(forward[1], backward[1]);
        assert_eq!(forward[2], backward[0]);
        let mut unique = forward.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 3);
        // The first of a pair keeps the plain id.
        assert_eq!(forward[0], finding_id(Category::Correctness, "f", "x / y"));
    }

    proptest! {
        #[test]
        fn whitespace_does_not_change_an_id(
            words in proptest::collection::vec("[a-z]{1,5}", 1..6),
            pad in proptest::collection::vec(" {1,3}|\t|\n", 6),
        ) {
            let tight = words.join(" ");
            let mut loose = String::new();
            for (i, w) in words.iter().enumerate() {
                loose.push_str(&pad[i % pad.len()]);
                loose.push_str(w);
            }
            loose.push_str(&pad[0]);
            prop_assert_eq!(normalise(&tight), normalise(&loose));
            prop_assert_eq!(
                finding_id(Category::Concurrency, "e", &normalise(&tight)),
                finding_id(Category::Concurrency, "e", &normalise(&loose))
            );
        }

        #[test]
        fn another_category_or_entity_changes_the_id(entity in "[a-z:]{1,8}", other in "[a-z:]{1,8}") {
            prop_assume!(entity != other);
            prop_assert_ne!(
                finding_id(Category::Correctness, &entity, "c"),
                finding_id(Category::Correctness, &other, "c")
            );
            prop_assert_ne!(
                finding_id(Category::Correctness, &entity, "c"),
                finding_id(Category::Performance, &entity, "c")
            );
        }
    }
}
