// SPDX-License-Identifier: AGPL-3.0-or-later

//! Config and cache paths from an env map (LOCAL_DEPLOYMENT §5). No process env.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gitcko_config::paths::{GitckoPaths, PathError};
use gitcko_redact::canaries;

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[cfg(unix)]
#[test]
fn home_defaults_follow_xdg() {
    let p = GitckoPaths::from_env(&env(&[("HOME", "/home/dev")])).unwrap();
    assert_eq!(
        p.config_file,
        PathBuf::from("/home/dev/.config/gitcko/config.toml")
    );
    assert_eq!(
        p.credentials_file,
        PathBuf::from("/home/dev/.config/gitcko/credentials.toml")
    );
    assert_eq!(p.cache_dir, PathBuf::from("/home/dev/.cache/gitcko"));
    assert_eq!(
        p.logs_dir,
        PathBuf::from("/home/dev/.local/state/gitcko/logs")
    );
}

#[cfg(unix)]
#[test]
fn xdg_dirs_win_over_home() {
    let p = GitckoPaths::from_env(&env(&[
        ("HOME", "/home/dev"),
        ("XDG_CONFIG_HOME", "/xdg/config"),
        ("XDG_CACHE_HOME", "/xdg/cache"),
        ("XDG_STATE_HOME", "/xdg/state"),
    ]))
    .unwrap();
    assert_eq!(
        p.config_file,
        PathBuf::from("/xdg/config/gitcko/config.toml")
    );
    assert_eq!(
        p.credentials_file,
        PathBuf::from("/xdg/config/gitcko/credentials.toml")
    );
    assert_eq!(p.cache_dir, PathBuf::from("/xdg/cache/gitcko"));
    assert_eq!(p.logs_dir, PathBuf::from("/xdg/state/gitcko/logs"));
}

#[cfg(unix)]
#[test]
fn gitcko_overrides_are_absolute_and_credentials_stay_in_the_config_dir() {
    let p = GitckoPaths::from_env(&env(&[
        ("HOME", "/home/dev"),
        ("GITCKO_CONFIG", "/tmp/custom.toml"),
        ("GITCKO_CACHE_DIR", "/tmp/cache"),
    ]))
    .unwrap();
    assert_eq!(p.config_file, PathBuf::from("/tmp/custom.toml"));
    assert_eq!(
        p.credentials_file,
        PathBuf::from("/home/dev/.config/gitcko/credentials.toml")
    );
    assert_eq!(p.cache_dir, PathBuf::from("/tmp/cache"));
}

#[cfg(unix)]
#[test]
fn overrides_make_home_optional() {
    let p = GitckoPaths::from_env(&env(&[
        ("GITCKO_CONFIG", "/abs/config.toml"),
        ("GITCKO_CACHE_DIR", "/abs/cache"),
        ("XDG_CONFIG_HOME", "/abs/configdir"),
        ("XDG_STATE_HOME", "/abs/state"),
    ]))
    .unwrap();
    assert_eq!(p.config_file, PathBuf::from("/abs/config.toml"));
    assert_eq!(
        p.credentials_file,
        PathBuf::from("/abs/configdir/gitcko/credentials.toml")
    );
    assert_eq!(p.cache_dir, PathBuf::from("/abs/cache"));
    assert_eq!(p.logs_dir, PathBuf::from("/abs/state/gitcko/logs"));
}

#[cfg(unix)]
#[test]
fn credentials_do_not_follow_a_config_override_when_home_is_unset() {
    assert_eq!(
        GitckoPaths::from_env(&env(&[
            ("GITCKO_CONFIG", "/abs/config.toml"),
            ("GITCKO_CACHE_DIR", "/abs/cache"),
            ("XDG_STATE_HOME", "/abs/state"),
        ]))
        .unwrap_err(),
        PathError::NoHome
    );
}

#[cfg(unix)]
#[test]
fn missing_home_is_a_typed_error() {
    assert_eq!(
        GitckoPaths::from_env(&BTreeMap::new()).unwrap_err(),
        PathError::NoHome
    );
}

#[test]
fn a_relative_override_is_rejected_without_echoing_its_value() {
    let canary = canaries()[1].clone();
    let mut vars = BTreeMap::new();
    #[cfg(unix)]
    {
        vars.insert("HOME".into(), "/home/dev".into());
    }
    #[cfg(windows)]
    {
        vars.insert("APPDATA".into(), r"C:\Users\dev\AppData\Roaming".into());
        vars.insert("LOCALAPPDATA".into(), r"C:\Users\dev\AppData\Local".into());
    }
    vars.insert("GITCKO_CONFIG".into(), format!("rel/{canary}.toml"));
    let err = GitckoPaths::from_env(&vars).unwrap_err();
    assert_eq!(
        err,
        PathError::NotAbsolute {
            var: "GITCKO_CONFIG"
        }
    );
    assert!(
        !err.to_string().contains(&canary),
        "error echoed the path value: {err}"
    );
}

#[cfg(windows)]
#[test]
fn windows_uses_appdata_and_localappdata() {
    let p = GitckoPaths::from_env(&env(&[
        ("APPDATA", r"C:\Users\dev\AppData\Roaming"),
        ("LOCALAPPDATA", r"C:\Users\dev\AppData\Local"),
    ]))
    .unwrap();
    assert_eq!(
        p.config_file,
        PathBuf::from(r"C:\Users\dev\AppData\Roaming\gitcko\config.toml")
    );
    assert_eq!(
        p.credentials_file,
        PathBuf::from(r"C:\Users\dev\AppData\Roaming\gitcko\credentials.toml")
    );
    assert_eq!(
        p.cache_dir,
        PathBuf::from(r"C:\Users\dev\AppData\Local\gitcko\cache")
    );
    assert_eq!(
        p.logs_dir,
        PathBuf::from(r"C:\Users\dev\AppData\Local\gitcko\logs")
    );
}
