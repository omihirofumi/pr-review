use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prpr::{
    keybindings::{Keybindings, Match, SPECS, Stroke, View, parse_sequence},
    settings::{self, Settings},
};
use std::collections::BTreeMap;

#[test]
fn defaults_are_conflict_free_and_printed_configuration_roundtrips() {
    let keys = Keybindings::default();
    keys.validate().unwrap();
    let parsed: Settings = toml::from_str(&keys.print().unwrap()).unwrap();
    parsed.keybindings.validate().unwrap();
    for spec in SPECS {
        assert_eq!(keys.keys(spec.id), parsed.keybindings.keys(spec.id));
    }
    assert_eq!(keys.keys("open_file"), ["gf", "e"]);
    assert!(matches!(
        keys.resolve(View::Diff, &parse_sequence("o").unwrap())
            .unwrap(),
        Match::None
    ));
    assert!(matches!(
        keys.resolve(View::Inbox, &parse_sequence("o").unwrap())
            .unwrap(),
        Match::Action("open_pr")
    ));
    assert!(matches!(
        keys.resolve(View::Overview, &parse_sequence("o").unwrap())
            .unwrap(),
        Match::Action("open_reading_step")
    ));
}

#[test]
fn configuration_supports_aliases_chords_disabling_and_context_specific_reuse() {
    let settings: Settings = toml::from_str(
        r#"
[keybindings]
command_palette = ["Ctrl-p", "F2"]
toggle_sidebar = ["Space b"]
next_file = []
next_hunk = ["n", "Alt-j"]
open_file = []
"#,
    )
    .unwrap();
    let keys = settings.keybindings;
    keys.validate().unwrap();
    assert!(matches!(
        keys.resolve(View::Diff, &parse_sequence("Space").unwrap())
            .unwrap(),
        Match::Prefix
    ));
    assert!(matches!(
        keys.resolve(View::Diff, &parse_sequence("Space b").unwrap())
            .unwrap(),
        Match::Action("toggle_sidebar")
    ));
    assert!(matches!(
        keys.resolve(View::Diff, &parse_sequence("n").unwrap())
            .unwrap(),
        Match::Action("next_hunk")
    ));
    assert!(matches!(
        keys.resolve(View::Overview, &parse_sequence("n").unwrap())
            .unwrap(),
        Match::Action("next_reading_step")
    ));
    assert!(matches!(
        keys.resolve(View::Inbox, &parse_sequence("Space b").unwrap())
            .unwrap(),
        Match::None
    ));
    assert!(matches!(
        keys.resolve(View::Diff, &parse_sequence("gf").unwrap())
            .unwrap(),
        Match::None
    ));
    assert_eq!(keys.display("open_file"), "Unbound");
    assert_eq!(keys.display("command_palette"), "Ctrl-p / F2");
}

#[test]
fn invalid_action_names_keys_duplicates_and_prefix_conflicts_are_rejected_before_saving() {
    for (id, bindings) in [
        ("nexthunk", vec!["]"]),
        ("command_palette", vec![""]),
        ("command_palette", vec!["Ctrl-"]),
        ("command_palette", vec!["F25"]),
        ("command_palette", vec!["Shift-/"]),
        ("command_palette", vec!["Ctrl-c"]),
        ("command_palette", vec!["g Esc"]),
        ("command_palette", vec!["Ctrl-k", "Ctrl-K"]),
        ("command_palette", vec!["g"]),
        ("next_hunk", vec!["n"]),
        ("open_file", vec!["gf", "g f"]),
    ] {
        let mut settings = Settings::default();
        settings
            .keybindings
            .0
            .insert(id.into(), bindings.into_iter().map(str::to_owned).collect());
        assert!(settings.keybindings.validate().is_err(), "{id}");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, "[ai]\nprovider = 'codex'\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(settings::save(&path, &settings).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn terminal_shift_and_backtab_events_match_their_configured_spellings() {
    assert_eq!(
        parse_sequence("Shift-q").unwrap(),
        vec![Stroke::from_event(KeyEvent::new(
            KeyCode::Char('Q'),
            KeyModifiers::SHIFT
        ))]
    );
    assert_eq!(
        parse_sequence("Shift-Tab").unwrap(),
        vec![Stroke::from_event(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::NONE
        ))]
    );
    assert_eq!(
        parse_sequence("Shift-Tab").unwrap(),
        vec![Stroke::from_event(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::SHIFT
        ))]
    );
    let keys = Keybindings(BTreeMap::from([(
        "toggle_sidebar".into(),
        vec!["Ctrl-x".into()],
    )]));
    assert!(keys.single_matches(
        "toggle_sidebar",
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
    ));
}

#[test]
fn terminal_control_aliases_are_validated_against_named_keys() {
    for (control, named) in [
        ("Ctrl-i", "Tab"),
        ("Ctrl-j", "Enter"),
        ("Ctrl-m", "Enter"),
        ("Ctrl-h", "Backspace"),
        ("Ctrl-[", "Esc"),
    ] {
        assert_eq!(
            parse_sequence(control).unwrap(),
            parse_sequence(named).unwrap()
        );
    }
    let mut keys = Keybindings::default();
    keys.0
        .insert("command_palette".into(), vec!["Ctrl-i".into()]);
    assert!(keys.validate().is_err());
}
