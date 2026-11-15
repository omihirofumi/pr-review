use prpr::{
    local::{self, Spec},
    model::*,
    notes,
};
use std::{path::Path, process::Command};

fn jj(root: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .current_dir(root)
        .args(["--no-pager", "--color=never"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    jj(directory.path(), &["git", "init", "."]);
    std::fs::write(directory.path().join("a.rs"), "before\r\nshared\r\n").unwrap();
    std::fs::write(directory.path().join("removed"), "remove me\n").unwrap();
    std::fs::write(directory.path().join("rename me"), "keep this\n").unwrap();
    jj(directory.path(), &["new", "-m", "Change behavior"]);
    std::fs::write(directory.path().join("a.rs"), "after\r\nshared\r\n").unwrap();
    std::fs::write(directory.path().join("odd\nname"), "no final newline").unwrap();
    std::fs::write(directory.path().join("empty"), "").unwrap();
    std::fs::write(directory.path().join("binary"), [0, 1, 0, 2]).unwrap();
    std::fs::remove_file(directory.path().join("removed")).unwrap();
    std::fs::rename(
        directory.path().join("rename me"),
        directory.path().join("renamed"),
    )
    .unwrap();
    directory
}

#[test]
fn jj_snapshot_preserves_source_coordinates_renames_eof_and_binary_inventory() {
    let directory = fixture();
    let spec = Spec::Jj {
        repository: directory.path().into(),
        revision: None,
        from: None,
        to: None,
    };
    let snapshot = spec.load().unwrap();
    assert!(snapshot.data.source.is_local());
    let content = &snapshot.contents["a.rs"];
    assert_eq!(content.base, "before\r\nshared\r\n");
    assert_eq!(content.head, "after\r\nshared\r\n");
    assert_eq!(snapshot.contents["odd\nname"].head, "no final newline");
    assert_eq!(snapshot.contents["removed"].base, "remove me\n");
    assert_eq!(snapshot.contents["removed"].head, "");
    assert!(
        snapshot
            .data
            .files
            .iter()
            .any(|file| file.filename == "binary" && file.patch.is_none())
    );
    assert_eq!(snapshot.contents["renamed"].head, "keep this\n");
    assert_eq!(snapshot.contents["empty"].head, "");
    std::fs::write(directory.path().join("a.rs"), "another version\n").unwrap();
    assert_eq!(snapshot.contents["a.rs"].head, "after\r\nshared\r\n");
    assert_ne!(
        spec.load().unwrap().data.pr.head_sha,
        snapshot.data.pr.head_sha
    );
    let parent = Spec::Jj {
        repository: directory.path().into(),
        revision: Some("@-".into()),
        from: None,
        to: None,
    }
    .load()
    .unwrap();
    assert_eq!(parent.contents["a.rs"].head, "before\r\nshared\r\n");
    let range = Spec::Jj {
        repository: directory.path().into(),
        revision: None,
        from: Some("@-".into()),
        to: Some("@".into()),
    }
    .load()
    .unwrap();
    assert_eq!(range.contents["a.rs"].head, "another version\n");
}

#[test]
fn directory_adapter_supports_ranges_and_notes_survive_restart_but_expire_after_source_changes() {
    let root = tempfile::tempdir().unwrap();
    let left = root.path().join("left");
    let right = root.path().join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    std::fs::write(left.join("source"), "old\nunchanged\n").unwrap();
    std::fs::write(right.join("source"), "new\nunchanged\n").unwrap();
    let spec = Spec::Directories {
        repository: root.path().into(),
        left,
        right: right.clone(),
    };
    std::fs::write(right.join("large"), vec![b'x'; 2 * 1024 * 1024 + 1]).unwrap();
    let snapshot = spec.load().unwrap();
    assert!(
        snapshot
            .data
            .files
            .iter()
            .any(|file| file.filename == "large" && file.patch.is_none())
    );
    let data = &snapshot.data;
    let mut notes = Vec::new();
    let anchor = DraftComment {
        path: "source".into(),
        line: 2,
        side: Side::Right,
        start_line: Some(1),
        start_side: Some(Side::Right),
        body: "Why change this?".into(),
    };
    let id = notes::add(&mut notes, data, anchor.clone(), None, "user".into()).unwrap();
    let mut reply = anchor;
    reply.body = "This changes the behavior.".into();
    notes::add(&mut notes, data, reply, Some(id), "codex".into()).unwrap();
    notes::save(root.path(), data, &notes).unwrap();
    let loaded = notes::load(root.path(), data).unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(notes::update(root.path(), data, &[], &[]).is_err());
    assert_eq!(notes::load(root.path(), data).unwrap().len(), 2);
    assert!(!loaded[0].outdated);
    assert!(notes::comments(&loaded)[1].in_reply_to_id.is_some());
    std::fs::write(right.join("source"), "newer\nunchanged\n").unwrap();
    let changed = spec.load().unwrap();
    assert!(
        notes::load(root.path(), &changed.data)
            .unwrap()
            .iter()
            .all(|note| note.outdated)
    );
    assert!(
        prpr::github::submit_review(data, &[], "", ReviewEvent::Comment)
            .unwrap_err()
            .to_string()
            .contains("Local reviews")
    );
}

#[cfg(unix)]
#[test]
fn directory_adapter_never_follows_source_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let left = root.path().join("left");
    let right = root.path().join("right");
    std::fs::create_dir_all(left.join("directory")).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    std::fs::write(left.join("directory/file"), "safe\n").unwrap();
    symlink("/etc", right.join("directory")).unwrap();
    let spec = Spec::Directories {
        repository: root.path().into(),
        left,
        right,
    };
    let snapshot = spec.load().unwrap();
    assert_eq!(snapshot.contents["directory/file"].head, "");
    assert_eq!(snapshot.contents["directory"].head, "/etc");
    assert!(local::validate_path("../outside").is_err());
}

#[cfg(unix)]
#[test]
fn jj_external_viewer_reattaches_the_terminal_and_exchanges_live_notes() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/difftool_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_prpr"))
        .arg(directory.path())
        .arg(state.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn external_viewer_uses_remapped_keys_without_changing_comment_editor_controls() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/difftool_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_prpr"))
        .arg(directory.path())
        .arg(state.path())
        .arg("custom")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
