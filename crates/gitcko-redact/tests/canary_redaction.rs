// SPDX-License-Identifier: AGPL-3.0-or-later

//! Canary tests: fake secrets and source markers pushed through redaction must never come
//! out the other side. `cargo xtask canaries` runs every test whose name contains
//! `canary`, in every crate.

use std::path::Path;

use gitcko_redact::{Redactor, canaries};
use proptest::prelude::*;

#[test]
fn canary_every_canary_is_redacted_by_shape_alone() {
    // No canary is registered as a known secret here: each must be caught by its shape,
    // the way an unknown key in a model's output or an error message would be.
    let redactor = Redactor::new();
    for canary in canaries() {
        let text = format!("request failed: token={canary} (retrying)");
        let out = redactor.redact(&text);
        assert!(!out.contains(canary.as_str()), "{canary:?} survived: {out}");
        assert!(
            out.contains("request failed") && out.contains("retrying"),
            "{out}"
        );
    }
}

#[test]
fn canary_there_is_one_per_secret_shape() {
    // A canary list that shrank would quietly weaken every sink test that uses it.
    assert!(canaries().len() >= 8);
}

#[test]
fn canary_registered_secret_is_redacted_whatever_its_shape() {
    let redactor = Redactor::new().secret("correct horse battery staple");
    let out = redactor.redact("password is correct horse battery staple, ok");
    assert_eq!(out, "password is <secret>, ok");
}

#[test]
fn longer_secrets_win_over_their_prefixes() {
    let redactor = Redactor::new()
        .secret("abcd1234")
        .secret("abcd1234efgh5678");
    assert_eq!(redactor.redact("k=abcd1234efgh5678"), "k=<secret>");
}

#[test]
fn empty_secret_is_ignored_rather_than_redacting_everything() {
    let redactor = Redactor::new().secret("");
    assert_eq!(redactor.redact("plain text"), "plain text");
}

#[test]
fn local_paths_become_placeholders() {
    // OBSERVABILITY §6: no unrestricted local paths in logs or errors.
    let home = Path::new(if cfg!(windows) {
        r"C:\Users\ana"
    } else {
        "/home/ana"
    });
    let repo = home.join("work").join("shop");
    let redactor = Redactor::new().path(&repo, "repo").path(home, "home");
    let text = format!(
        "cannot read {} or {}",
        repo.join("src").join("a.rs").display(),
        home.join(".cache").display()
    );
    let out = redactor.redact(&text);
    assert!(!out.contains("ana"), "{out}");
    assert!(out.contains("<repo>") && out.contains("<home>"), "{out}");
}

#[test]
fn windows_paths_are_redacted_with_either_separator() {
    let redactor = Redactor::new().path(Path::new(r"C:\Users\ana"), "home");
    assert_eq!(
        redactor.redact(r"C:\Users\ana\x and C:/Users/ana/y"),
        r"<home>\x and <home>/y"
    );
}

#[test]
fn ordinary_text_is_left_alone() {
    let text = "Review rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X found 2 findings in src/retry.ts:42.";
    assert_eq!(Redactor::new().redact(text), text);
}

proptest! {
    #[test]
    fn canary_never_survives_anywhere_in_text(
        which in 0..canaries().len(),
        before in "[ -~\n]{0,40}",
        after in "[ -~\n]{0,40}",
    ) {
        let canary = canaries()[which].clone();
        let text = format!("{before}{canary}{after}");
        let out = Redactor::new().redact(&text);
        prop_assert!(!out.contains(canary.as_str()), "{}", out);
    }
}

#[test]
fn canary_url_keeps_host_but_drops_credentials() {
    let out = Redactor::new()
        .redact("connect postgres://gitcko:CanaryPassw0rd@db.example.invalid:5432/app");
    assert_eq!(
        out,
        "connect postgres://<secret>@db.example.invalid:5432/app"
    );
}

#[test]
fn canary_overlapping_secrets_leave_no_piece_behind() {
    // Replacing one secret at a time would cut the second one in half and leak its tail.
    let redactor = Redactor::new().secret("abcd1234XYZ").secret("XYZ5678efgh");
    assert_eq!(redactor.redact("k=abcd1234XYZ5678efgh!"), "k=<secret>!");
}

#[test]
fn canary_truncated_private_key_is_still_redacted() {
    // An error message or a cut-off log line can hold a key with no END line.
    let out = Redactor::new()
        .redact("bad key: -----BEGIN PRIVATE KEY-----\nMIICanaryCanaryCanaryQQQQ\nMIIC");
    assert_eq!(out, "bad key: <secret>");
}

#[test]
fn canary_url_userinfo_of_any_shape_is_dropped() {
    for (text, want) in [
        (
            "https://CanaryTok3nOnly1234@git.example.invalid/r.git",
            "https://<secret>@git.example.invalid/r.git",
        ),
        (
            "mysql://app:Can@ry/Pass@db.example.invalid/app",
            "mysql://<secret>@db.example.invalid/app",
        ),
    ] {
        assert_eq!(Redactor::new().redact(text), want);
    }
}

#[test]
fn canary_registered_value_inside_a_shaped_token_takes_the_whole_token() {
    // Replacing the registered part first would break the Bearer shape and leave the
    // rest of the token readable.
    let redactor = Redactor::new().secret("abc123def456");
    let out = redactor.redact("Authorization: Bearer ghi789abc123def456jkl012");
    assert_eq!(out, "Authorization: Bearer <secret>");
}

#[test]
fn canary_short_and_password_only_credentials_are_redacted() {
    let redactor = Redactor::new();
    assert_eq!(
        redactor.redact("Authorization: Bearer aB3dE5fG7hJ9"),
        "Authorization: Bearer <secret>"
    );
    assert_eq!(
        redactor.redact("redis://:CanaryPassw0rd@localhost:6379"),
        "redis://<secret>@localhost:6379"
    );
}

#[test]
fn canary_debug_output_hides_registered_values() {
    let redactor = Redactor::new()
        .secret("CanaryDebugSecret99")
        .path(Path::new("/home/canaryuser"), "home");
    let shown = format!("{redactor:?}");
    assert!(
        !shown.contains("CanaryDebugSecret99") && !shown.contains("canaryuser"),
        "{shown}"
    );
}
