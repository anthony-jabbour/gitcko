// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shell: header badges, the three screens, and a terminal that comes back
//! after a panic.

use std::collections::BTreeMap;
use std::panic;
use std::sync::{Arc, Mutex};

use gitcko_test_support::TempDir;
use gitcko_tui::{
    App, Color, Effect, Header, IndexState, KeyCode, KeyEvent, KeyModifiers, Locality, Screen,
    cell_fg, key, load, render_text,
};
use gitcko_tui::{Guard, Restorer};

fn snap(name: &str, app: &App) {
    for cols in [80u16, 120, 200] {
        let frame = render_text(app, cols, 24);
        insta::assert_snapshot!(format!("{name}_{cols}"), frame);
    }
}

#[test]
fn the_shell_shows_the_header_tabs_and_word_or_mark() {
    let app = App::preview();
    snap("shell_review", &app);
    let narrow = render_text(&app, 80, 24);
    assert!(narrow.contains("Gitcko"), "{narrow}");
    assert!(
        !narrow.contains('▄'),
        "a narrow header uses the word: {narrow}"
    );
    assert!(narrow.contains("acme/api"), "{narrow}");
    assert!(narrow.contains("feat/auth"), "{narrow}");
    assert!(narrow.contains("ready"), "{narrow}");
    assert!(narrow.contains("qwen3-coder"), "{narrow}");
    assert!(narrow.contains("local"), "{narrow}");
    assert!(narrow.contains("[1 Review]"), "{narrow}");
    let wide = render_text(&app, 120, 24);
    assert!(wide.contains("▄▀▄"), "a wide header uses the mark: {wide}");
    assert!(wide.contains("Gitcko"), "{wide}");
}

#[test]
fn findings_history_and_help_are_their_own_screens() {
    let mut findings = App::preview();
    findings.screen = Screen::Findings;
    snap("shell_findings", &findings);
    let mut history = App::preview();
    history.screen = Screen::History;
    snap("shell_history", &history);
    let mut help = App::preview();
    help.help = true;
    snap("shell_help", &help);
    assert!(render_text(&help, 80, 24).contains("Screen readers"));
}

#[test]
fn a_missing_repository_names_the_path_and_how_to_fix_it() {
    let mut app = App::preview();
    app.no_repo = Some("/work/notes".into());
    app.header.repo = "/work/notes".into();
    snap("shell_no_repo", &app);
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("No repository"), "{frame}");
    assert!(frame.contains("/work/notes"), "{frame}");
    assert!(frame.contains("git repository"), "{frame}");
}

#[test]
fn keys_switch_screens_and_quit_and_a_release_does_not() {
    let mut app = App::preview();
    assert_eq!(app.on_key(key(KeyCode::Char('2'))), Effect::None);
    assert_eq!(app.screen, Screen::Findings);
    assert_eq!(app.on_key(key(KeyCode::Tab)), Effect::None);
    assert_eq!(app.screen, Screen::History);
    assert_eq!(app.on_key(key(KeyCode::Tab)), Effect::None);
    assert_eq!(app.screen, Screen::Review);
    assert_eq!(app.on_key(key(KeyCode::Char('3'))), Effect::None);
    assert_eq!(app.screen, Screen::History);
    assert_eq!(app.on_key(key(KeyCode::Char('1'))), Effect::None);
    assert_eq!(app.screen, Screen::Review);

    app.on_key(key(KeyCode::Char('?')));
    assert!(app.help);
    app.on_key(key(KeyCode::Esc));
    assert!(!app.help);

    let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release.kind = gitcko_tui::KeyEventKind::Release;
    assert_eq!(app.on_key(release), Effect::None);
    assert!(!app.quit);

    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::Quit);
    assert!(app.quit);
}

#[test]
fn ctrl_c_on_an_idle_shell_quits_and_slash_commands_move() {
    let mut app = App::preview();
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.on_key(ctrl_c), Effect::Quit);
    assert!(app.quit);

    let mut app = App::preview();
    app.on_key(key(KeyCode::Char('/')));
    app.on_key(key(KeyCode::Char('f')));
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(key(KeyCode::Char('n')));
    app.on_key(key(KeyCode::Char('d')));
    app.on_key(key(KeyCode::Char('i')));
    app.on_key(key(KeyCode::Char('n')));
    app.on_key(key(KeyCode::Char('g')));
    app.on_key(key(KeyCode::Char('s')));
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::None);
    assert_eq!(app.screen, Screen::Findings);

    app.on_key(key(KeyCode::Char('/')));
    for ch in ['a', 's', 'k'] {
        app.on_key(key(KeyCode::Char(ch)));
    }
    app.on_key(key(KeyCode::Enter));
    assert!(app.status.contains("not in this build"), "{}", app.status);

    app.on_key(key(KeyCode::Char('/')));
    for ch in ['n', 'o', 'p', 'e'] {
        app.on_key(key(KeyCode::Char(ch)));
    }
    app.on_key(key(KeyCode::Enter));
    assert!(app.status.contains("unknown command"), "{}", app.status);
}

#[test]
fn a_failed_index_is_red_and_no_color_is_not_the_control() {
    let mut app = App::preview();
    app.header.index = IndexState::Failed {
        reason: "the index is corrupt".into(),
    };
    assert_eq!(
        cell_fg(&app, 120, 24, "failed"),
        Some(Color::Red),
        "a failed index is a problem badge"
    );
    let ready = App::preview();
    assert_ne!(
        cell_fg(&ready, 120, 24, "ready"),
        Some(Color::Red),
        "a ready index is the control: it is not a problem"
    );
    app.color = false;
    assert_ne!(
        cell_fg(&app, 120, 24, "failed"),
        Some(Color::Red),
        "NO_COLOR drops the red"
    );
    assert!(render_text(&app, 120, 24).contains("failed"));
}

#[test]
fn a_remote_model_is_named_in_the_header() {
    let mut app = App::preview();
    app.header.locality = Locality::Remote {
        provider: "openrouter".into(),
    };
    app.header.model = "anthropic/claude-haiku-5.5".into();
    let frame = render_text(&app, 200, 24);
    assert!(frame.contains("remote: openrouter"), "{frame}");
    assert!(frame.contains("anthropic/claude-haiku-5.5"), "{frame}");
    assert!(!frame.contains("local)"), "{frame}");
}

#[test]
fn loading_a_repository_fills_the_branch_and_a_missing_model() {
    let repo = TempDir::new("tui-shell");
    let bare = repo.path().join("empty");
    std::fs::create_dir_all(&bare).unwrap();
    let cache = repo.path().join("cache");
    let config = repo.path().join("no-config.toml");
    std::fs::create_dir_all(&cache).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), repo.path().display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
    ]);
    let missing = load(&bare, &env);
    assert!(missing.no_repo.is_some(), "{missing:?}");
    assert!(render_text(&missing, 80, 24).contains("No repository"));

    let git = gitcko_test_support::empty_repo("tui-branch");
    let mut config_text = std::fs::read_to_string(git.path().join(".git/config")).unwrap();
    config_text.push_str("\n[remote \"origin\"]\n\turl = git@github.com:acme/api.git\n");
    std::fs::write(git.path().join(".git/config"), config_text).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), repo.path().display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
    ]);
    let loaded = load(git.path(), &env);
    assert!(loaded.no_repo.is_none(), "{loaded:?}");
    assert_eq!(loaded.header.repo, "acme/api");
    assert_eq!(loaded.header.branch, "main");
    assert_eq!(loaded.header.index, IndexState::Missing);
    assert_eq!(loaded.header.model, "none");
    assert!(
        loaded.status.contains("gitcko setup"),
        "no model tells the user what to run: {}",
        loaded.status
    );
}

#[test]
fn no_color_in_the_environment_turns_colour_off_and_force_color_is_the_control() {
    let dir = TempDir::new("tui-color");
    let cache = dir.path().join("cache");
    let config = dir.path().join("no-config.toml");
    std::fs::create_dir_all(&cache).unwrap();
    let mut off = BTreeMap::from([
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        ("NO_COLOR".into(), "1".into()),
    ]);
    let app = load(dir.path(), &off);
    assert!(!app.color);
    off.insert("FORCE_COLOR".into(), "1".into());
    let forced = load(dir.path(), &off);
    assert!(forced.color, "FORCE_COLOR wins over NO_COLOR");
}

#[test]
fn the_cache_id_is_stable_for_one_directory() {
    let dir = TempDir::new("tui-id");
    let once = gitcko_tui::repo_cache_id(dir.path()).expect("canonical path");
    let twice = gitcko_tui::repo_cache_id(dir.path()).expect("canonical path");
    assert_eq!(once, twice);
    assert_eq!(once.len(), 64);
}

struct LogRestorer {
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Restorer for LogRestorer {
    fn restore(&mut self) {
        self.log.lock().expect("log").push("restore");
    }
}

#[test]
fn the_terminal_guard_restores_on_the_way_out_and_after_a_panic() {
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let _guard = Guard::new(LogRestorer { log: log.clone() });
    }
    assert_eq!(log.lock().expect("log").as_slice(), ["restore"]);

    let log = Arc::new(Mutex::new(Vec::new()));
    let caught = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _guard = Guard::new(LogRestorer { log: log.clone() });
        panic!("draw failed");
    }));
    assert!(caught.is_err(), "the panic still propagates");
    assert_eq!(
        log.lock().expect("log").as_slice(),
        ["restore"],
        "drop still ran"
    );
}

#[test]
fn header_pieces_name_every_badge() {
    let line = gitcko_tui::render_text(&App::preview(), 200, 24);
    assert!(line.contains("repo:"));
    assert!(line.contains("branch:"));
    assert!(line.contains("index:"));
    assert!(line.contains("model:"));
    let mut building = App::preview();
    building.header = Header::preview();
    building.header.index = IndexState::Building;
    assert!(render_text(&building, 120, 24).contains("building"));
    building.header.index = IndexState::Stale;
    assert!(render_text(&building, 120, 24).contains("stale"));
}
