// SPDX-License-Identifier: AGPL-3.0-or-later

//! Author family for model inversion (MODEL_ROUTING §9): trailers, then branch, then
//! title; majority of commits; anything unclear is unknown.

use gitcko_model::{AgentFamilies, AuthorFamily, AuthorSignals, Signal, author_family};
use gitcko_protocol::config::ModelFamily::{Anthropic, Openai};

fn trailer(domain: &str) -> String {
    format!("Fix the thing\n\nCo-authored-by: Some Agent <noreply@{domain}>\n")
}

fn infer(commits: &[String], branch: &str, title: &str) -> AuthorFamily {
    author_family(
        &AuthorSignals {
            commit_messages: commits.to_vec(),
            branch: branch.into(),
            title: title.into(),
        },
        &AgentFamilies::default(),
    )
}

fn known(family: gitcko_protocol::config::ModelFamily, signal: Signal) -> AuthorFamily {
    AuthorFamily::Known { family, signal }
}

#[test]
fn author_family_table() {
    let a = trailer("anthropic.com");
    let o = trailer("openai.com");
    let plain = "Fix the thing".to_string();
    let cases: &[(&[String], &str, &str, AuthorFamily)] = &[
        (
            std::slice::from_ref(&a),
            "",
            "",
            known(Anthropic, Signal::Trailer),
        ),
        // Two of three commits: a majority.
        (
            &[a.clone(), a.clone(), plain.clone()],
            "",
            "",
            known(Anthropic, Signal::Trailer),
        ),
        // One of two is not more than half.
        (&[a.clone(), plain.clone()], "", "", AuthorFamily::Unknown),
        // Conflicting trailers.
        (&[a.clone(), o.clone()], "", "", AuthorFamily::Unknown),
        // Trailers outrank the branch, and a trailer split doesn't fall through to it.
        (
            std::slice::from_ref(&o),
            "claude/fix",
            "",
            known(Openai, Signal::Trailer),
        ),
        (
            &[a.clone(), o.clone()],
            "claude/fix",
            "",
            AuthorFamily::Unknown,
        ),
        // No trailers: branch, then title.
        (
            std::slice::from_ref(&plain),
            "codex/fix-thing",
            "[claude] Fix",
            known(Openai, Signal::BranchPrefix),
        ),
        (
            std::slice::from_ref(&plain),
            "feature/x",
            "[Claude] Fix",
            known(Anthropic, Signal::TitlePrefix),
        ),
        // Agents whose model the user picks, and unknown domains, say nothing.
        (
            &[trailer("example.test")],
            "cursor/x",
            "[devin] y",
            AuthorFamily::Unknown,
        ),
        (&[], "", "", AuthorFamily::Unknown),
        // A prefix must be the whole first segment, not a substring.
        (
            std::slice::from_ref(&plain),
            "claudette/x",
            "",
            AuthorFamily::Unknown,
        ),
        (
            std::slice::from_ref(&plain),
            "",
            "[claude-ish] x",
            AuthorFamily::Unknown,
        ),
        // A look-alike domain is not the vendor's, before or after the real name.
        (
            &[trailer("notanthropic.com")],
            "",
            "",
            AuthorFamily::Unknown,
        ),
        (
            &[trailer("anthropic.com.evil.test")],
            "",
            "",
            AuthorFamily::Unknown,
        ),
    ];
    for (commits, branch, title, expected) in cases {
        assert_eq!(
            infer(commits, branch, title),
            *expected,
            "{commits:?} {branch} {title}"
        );
    }
}

#[test]
fn trailer_keys_and_subdomains_match_like_git() {
    let lower = "x\n\nco-authored-by: A <bot@mail.anthropic.com>\n".to_string();
    assert_eq!(infer(&[lower], "", ""), known(Anthropic, Signal::Trailer));
    // Only trailer lines count, not the word in the body.
    let body = "Mentions Co-authored-by: noreply@openai.com in prose".to_string();
    assert_eq!(infer(&[body], "", ""), AuthorFamily::Unknown);
}

#[test]
fn only_the_final_trailer_block_counts() {
    // A trailer quoted in the body, followed by prose, is not a trailer (git reads
    // trailers from the message's last paragraph only).
    let quoted =
        "Fix\n\nCo-authored-by: Bot <bot@anthropic.com>\n\nThat line above is an example.\n"
            .to_string();
    assert_eq!(
        infer(&[quoted], "codex/x", ""),
        known(Openai, Signal::BranchPrefix)
    );
    // CRLF line ends, a blank line holding spaces, and trailing blank lines.
    let crlf = "Fix\r\n\r\nCo-authored-by: A <a@openai.com>\r\n\r\n".to_string();
    assert_eq!(infer(&[crlf], "", ""), known(Openai, Signal::Trailer));
    let spaced = "Co-authored-by: A <a@openai.com>\n  \nprose\n".to_string();
    assert_eq!(infer(&[spaced], "", ""), AuthorFamily::Unknown);
}
