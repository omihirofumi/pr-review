use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};

use crate::model::{ChangedFile, PullRequest, Side};

pub fn prepare(pr: &PullRequest, root: &Path) -> Result<PathBuf> {
    validate_pr(pr)?;
    prepare_from_remote(
        pr,
        root,
        OsStr::new(&format!("https://github.com/{}", pr.id.repo)),
    )
}

fn prepare_from_remote(pr: &PullRequest, root: &Path, remote: &OsStr) -> Result<PathBuf> {
    validate_pr(pr)?;
    fs::create_dir_all(root).context("Cannot create the review workspace directory")?;
    let root = root.canonicalize()?;
    let store = root.join("repos").join(&pr.id.repo);
    let workspace = root
        .join("workspaces")
        .join(&pr.id.repo)
        .join(format!("pr-{}-{}", pr.id.number, pr.head_sha));

    create_directories(
        &root,
        Path::new("repos").join(&pr.id.repo).parent().unwrap(),
    )?;
    reject_symlinks(&root, store.strip_prefix(&root)?)?;
    if !store.join(".jj").exists() {
        ensure!(
            !store.exists(),
            "{} already exists and is not a Jujutsu repository",
            store.display()
        );
        run(jj(None).args(["git", "init", "--no-colocate"]).arg(&store))?;
    }
    ensure_contained(&root, &store)?;
    reject_symlinks(&store, Path::new(".jj"))?;
    let remotes = run(jj(Some(&store)).args(["git", "remote", "list"]))?;
    let operation = if remotes
        .lines()
        .any(|line| line.split_whitespace().next() == Some("origin"))
    {
        "set-url"
    } else {
        "add"
    };
    run(jj(Some(&store))
        .args(["git", "remote", operation, "origin"])
        .arg(remote))?;
    let git_dir = git_dir(&store)?;
    let reference = format!("refs/heads/pr-review-{}", pr.id.number);
    let refspec = format!("+refs/pull/{}/head:{reference}", pr.id.number);
    let mut fetch = git(&git_dir);
    fetch
        .args(["fetch", "--quiet", "--no-tags"])
        .arg(remote)
        .arg(refspec);
    run(&mut fetch)
        .context("Cannot fetch the pull request; check gh authentication and repository access")?;
    let fetched = run(git(&git_dir).args(["rev-parse", &reference]))?;
    ensure!(
        fetched.trim() == pr.head_sha,
        "The pull request changed since it was loaded; refresh before checking out"
    );
    if output(git(&git_dir).args(["cat-file", "-e", &format!("{}^{{commit}}", pr.base_sha)]))
        .is_err()
    {
        run(git(&git_dir)
            .args(["fetch", "--quiet", "--no-tags"])
            .arg(remote)
            .arg(&pr.base_sha))
        .context("Cannot fetch the diff's base revision")?;
    }
    run(jj(Some(&store)).args(["git", "import"]))?;

    create_directories(&root, workspace.parent().unwrap().strip_prefix(&root)?)?;
    reject_symlinks(&root, workspace.strip_prefix(&root)?)?;
    if !workspace.exists() {
        let name = format!("pr-{}-{}", pr.id.number, pr.head_sha);
        run(jj(Some(&store))
            .args([
                "workspace",
                "add",
                "--no-colocate",
                "--name",
                &name,
                "-r",
                &pr.head_sha,
            ])
            .arg(&workspace))?;
    }
    ensure_contained(&root, &workspace)?;
    validate_workspace(pr, &workspace)?;
    Ok(workspace)
}

pub fn editor_location(
    pr: &PullRequest,
    file: &ChangedFile,
    side: &Side,
    line: u32,
    workspace: &Path,
) -> Result<PathBuf> {
    validate_pr(pr)?;
    ensure!(line > 0, "Choose a numbered diff line before opening Helix");
    validate_workspace(pr, workspace)?;
    let relative = match side {
        Side::Left => file.previous_filename.as_deref().unwrap_or(&file.filename),
        Side::Right => &file.filename,
    };
    validate_path(relative)?;
    if *side == Side::Right {
        let path = workspace.join(relative);
        reject_symlinks(workspace, Path::new(relative))?;
        ensure!(
            path.is_file(),
            "The selected file does not exist at the PR head"
        );
        ensure_contained(workspace, &path)?;
        return Ok(path.canonicalize()?);
    }

    let fileset = format!("root-file:{}", serde_json::to_string(relative)?);
    let bytes =
        output(jj(Some(workspace)).args(["file", "show", "-r", &pr.base_sha, "--", &fileset]))?;
    let snapshot_root = create_directories(
        workspace,
        &Path::new(".jj").join("pr-review-base").join(&pr.base_sha),
    )?;
    let snapshot = snapshot_root.join(relative);
    create_directories(&snapshot_root, Path::new(relative).parent().unwrap())?;
    reject_symlinks(&snapshot_root, Path::new(relative))?;
    if snapshot.exists() {
        ensure!(
            fs::read(&snapshot)? == bytes,
            "The saved base snapshot was modified; remove {} and retry",
            snapshot.display()
        );
    } else {
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&snapshot)?;
        destination.write_all(&bytes)?;
        let mut permissions = destination.metadata()?.permissions();
        permissions.set_readonly(true);
        destination.set_permissions(permissions)?;
    }
    Ok(snapshot.canonicalize()?)
}

fn validate_workspace(pr: &PullRequest, workspace: &Path) -> Result<()> {
    ensure!(
        !fs::symlink_metadata(workspace)?.file_type().is_symlink(),
        "Refusing a symlink as the review workspace"
    );
    reject_symlinks(workspace, Path::new(".jj"))?;
    let parent = run(jj(Some(workspace)).args([
        "log",
        "--no-graph",
        "-r",
        "@-",
        "-T",
        "commit_id ++ \"\\n\"",
    ]))?;
    ensure!(
        parent.trim() == pr.head_sha,
        "The review workspace is on a different revision; its contents were preserved at {}",
        workspace.display()
    );
    let changes = run(jj(Some(workspace)).args(["diff", "--summary", "-r", "@"]))?;
    ensure!(
        changes.trim().is_empty(),
        "The review workspace has local changes; its contents were preserved at {}. Save or move those changes before opening the reviewed revision",
        workspace.display()
    );
    Ok(())
}

fn validate_pr(pr: &PullRequest) -> Result<()> {
    let parts = pr.id.repo.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && *part != "."
                    && *part != ".."
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            }),
        "Expected a repository in OWNER/REPO form"
    );
    ensure!(pr.id.number > 0, "Pull request numbers must be positive");
    for sha in [&pr.head_sha, &pr.base_sha] {
        ensure!(
            (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "Expected a full commit SHA"
        );
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "The selected file has no path");
    ensure!(
        Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "The selected file path must stay inside its repository"
    );
    ensure!(
        !path.contains('\0'),
        "The selected file path contains a NUL byte"
    );
    Ok(())
}

fn ensure_contained(root: &Path, path: &Path) -> Result<()> {
    ensure!(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "The selected path escapes the review workspace"
    );
    Ok(())
}

fn create_directories(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = root.canonicalize()?;
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "Directory paths must stay inside the review cache"
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Refusing a symlink or non-directory in the review cache: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn reject_symlinks(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "Cannot open a symlink as reviewed source code: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn jj(repository: Option<&Path>) -> Command {
    let mut command = Command::new("jj");
    command.args(["--no-pager", "--color=never"]);
    if let Some(repository) = repository {
        command.current_dir(repository);
    }
    command.stdin(Stdio::null());
    command
}

fn git(git_dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("--git-dir").arg(git_dir);
    command.args([
        "-c",
        "credential.helper=",
        "-c",
        "credential.helper=!gh auth git-credential",
    ]);
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1");
    command.stdin(Stdio::null());
    command
}

fn git_dir(repository: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        run(jj(Some(repository)).args(["git", "root"]))?.trim(),
    ))
}

fn run(command: &mut Command) -> Result<String> {
    String::from_utf8(output(command)?).context("A command returned invalid UTF-8")
}

fn output(command: &mut Command) -> Result<Vec<u8>> {
    let name = command.get_program().to_string_lossy().into_owned();
    let result = bounded_output(command, Duration::from_secs(120))?;
    if !result.status.success() {
        bail!(
            "{name} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(result.stdout)
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Result<Output> {
    let name = command.get_program().to_string_lossy().into_owned();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot run {name}; ensure it is installed"))?;
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let started = Instant::now();
    let mut out = None;
    let mut err = None;
    loop {
        for (receiver, destination) in [(&stdout, &mut out), (&stderr, &mut err)] {
            if destination.is_none() {
                match receiver.try_recv() {
                    Ok(Ok(bytes)) => *destination = Some(bytes),
                    Ok(Err(error)) => {
                        terminate(&mut child);
                        return Err(error).with_context(|| format!("Cannot read {name} output"));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        terminate(&mut child);
                        bail!("Cannot read {name} output: reader stopped unexpectedly");
                    }
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) if out.is_some() && err.is_some() => {
                return Ok(Output {
                    status,
                    stdout: out.unwrap(),
                    stderr: err.unwrap(),
                });
            }
            Ok(_) => {}
            Err(error) => {
                terminate(&mut child);
                return Err(error).with_context(|| format!("Cannot wait for {name}"));
            }
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            bail!(
                "{name} timed out after {:.1} seconds; retry when the connection is available",
                timeout.as_secs_f64()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn drain(reader: impl Read + Send + 'static) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        const MAX_OUTPUT: u64 = 64 * 1024 * 1024;
        let mut bytes = Vec::new();
        let result = reader
            .take(MAX_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() as u64 > MAX_OUTPUT {
                    Err(std::io::Error::other("Command output exceeded 64 MiB"))
                } else {
                    Ok(bytes)
                }
            });
        let _ = sender.send(result);
    });
    receiver
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PrId;
    use tempfile::TempDir;

    struct Fixture {
        _directory: TempDir,
        source: PathBuf,
        root: PathBuf,
        pr: PullRequest,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            fs::create_dir(&source).unwrap();
            fixture_git(&source, &["init", "--initial-branch=main"]);
            fixture_git(&source, &["config", "user.name", "Review Test"]);
            fixture_git(&source, &["config", "user.email", "review@example.test"]);
            fs::write(source.join("old(name).rs"), "base source\n").unwrap();
            fs::create_dir(source.join("nested")).unwrap();
            fs::write(source.join("nested/base.rs"), "nested base source\n").unwrap();
            fixture_git(&source, &["add", "."]);
            fixture_git(&source, &["commit", "-m", "Create the diff base"]);
            let base_sha = fixture_git(&source, &["rev-parse", "HEAD"]);
            fs::rename(source.join("old(name).rs"), source.join("new name.rs")).unwrap();
            fs::write(source.join("new name.rs"), "reviewed source\n").unwrap();
            fixture_git(&source, &["add", "."]);
            fixture_git(&source, &["commit", "-m", "Provide a PR head for review"]);
            let head_sha = fixture_git(&source, &["rev-parse", "HEAD"]);
            fixture_git(&source, &["update-ref", "refs/pull/42/head", "HEAD"]);
            let pr = PullRequest {
                id: PrId {
                    repo: "test-org/review-fixture".into(),
                    number: 42,
                },
                title: "Fixture".into(),
                body: String::new(),
                url: "https://github.com/test-org/review-fixture/pull/42".into(),
                author: "test-user".into(),
                head_sha,
                base_sha,
                draft: false,
            };
            let root = directory.path().join("reviews");
            Self {
                _directory: directory,
                source,
                root,
                pr,
            }
        }
    }

    fn fixture_git(directory: &Path, args: &[&str]) -> String {
        run(Command::new("git").current_dir(directory).args(args))
            .unwrap()
            .trim()
            .into()
    }

    #[test]
    fn prepares_a_pinned_workspace_and_preserves_existing_work() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("new name.rs"), "ongoing local work\n").unwrap();
        let workspace =
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).unwrap();
        let remotes = run(jj(Some(&workspace)).args(["git", "remote", "list"])).unwrap();
        assert!(
            remotes.lines().any(|line| line.starts_with("origin ")
                && line.contains(fixture.source.to_str().unwrap()))
        );
        assert_eq!(
            fs::read_to_string(workspace.join("new name.rs")).unwrap(),
            "reviewed source\n"
        );
        assert_eq!(
            fs::read_to_string(fixture.source.join("new name.rs")).unwrap(),
            "ongoing local work\n"
        );
        assert_eq!(
            fixture_git(&fixture.source, &["rev-parse", "HEAD"]),
            fixture.pr.head_sha
        );
        assert_eq!(
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).unwrap(),
            workspace
        );

        let file = ChangedFile {
            filename: "new name.rs".into(),
            previous_filename: Some("old(name).rs".into()),
            status: "renamed".into(),
            additions: 1,
            deletions: 1,
            patch: None,
        };
        let right = editor_location(&fixture.pr, &file, &Side::Right, 1, &workspace).unwrap();
        assert_eq!(fs::read_to_string(right).unwrap(), "reviewed source\n");
        let left = editor_location(&fixture.pr, &file, &Side::Left, 1, &workspace).unwrap();
        assert_eq!(fs::read_to_string(&left).unwrap(), "base source\n");
        assert!(fs::metadata(left).unwrap().permissions().readonly());

        fs::write(workspace.join("new name.rs"), "local review edit\n").unwrap();
        let error = prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str())
            .unwrap_err();
        assert!(error.to_string().contains("local changes"));
        assert_eq!(
            fs::read_to_string(workspace.join("new name.rs")).unwrap(),
            "local review edit\n"
        );
        assert!(editor_location(&fixture.pr, &file, &Side::Right, 1, &workspace).is_err());
    }

    #[test]
    fn rejects_a_pr_that_moved_before_checkout() {
        let mut fixture = Fixture::new();
        fixture.pr.head_sha = fixture.pr.base_sha.clone();
        let error = prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str())
            .unwrap_err();
        assert!(error.to_string().contains("changed since"));
        assert!(!fixture.root.join("workspaces").exists());
    }

    #[test]
    fn rejects_paths_and_repositories_that_escape_the_cache() {
        assert!(validate_path("../secret").is_err());
        assert!(validate_path("/etc/passwd").is_err());
        assert!(validate_path("safe/../../secret").is_err());
        assert!(validate_path("safe/file.rs").is_ok());
        let mut fixture = Fixture::new();
        fixture.pr.id.repo = "../outside".into();
        assert!(
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).is_err()
        );
        assert!(!fixture.root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_files_and_directories() {
        let directory = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", directory.path().join("link")).unwrap();
        assert!(reject_symlinks(directory.path(), Path::new("link")).is_err());
        std::os::unix::fs::symlink("/etc", directory.path().join("nested")).unwrap();
        assert!(reject_symlinks(directory.path(), Path::new("nested/passwd")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_snapshot_symlinks_before_creating_outside_directories() {
        let fixture = Fixture::new();
        let workspace =
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = ChangedFile {
            filename: "new name.rs".into(),
            previous_filename: Some("nested/base.rs".into()),
            status: "renamed".into(),
            additions: 1,
            deletions: 1,
            patch: None,
        };
        let parent = workspace.join(".jj/pr-review-base");
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();
        assert!(editor_location(&fixture.pr, &file, &Side::Left, 1, &workspace).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        fs::remove_file(&parent).unwrap();

        fs::create_dir(&parent).unwrap();
        let snapshot_root = parent.join(&fixture.pr.base_sha);
        std::os::unix::fs::symlink(outside.path(), &snapshot_root).unwrap();
        assert!(editor_location(&fixture.pr, &file, &Side::Left, 1, &workspace).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        fs::remove_file(&snapshot_root).unwrap();

        fs::create_dir(&snapshot_root).unwrap();
        std::os::unix::fs::symlink(outside.path(), snapshot_root.join("nested")).unwrap();
        assert!(editor_location(&fixture.pr, &file, &Side::Left, 1, &workspace).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_cache_symlinks_before_creating_outside_directories() {
        let fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(&fixture.root).unwrap();
        let repos = fixture.root.join("repos");
        std::os::unix::fs::symlink(outside.path(), &repos).unwrap();
        assert!(
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).is_err()
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        fs::remove_file(&repos).unwrap();

        std::os::unix::fs::symlink(outside.path(), fixture.root.join("workspaces")).unwrap();
        assert!(
            prepare_from_remote(&fixture.pr, &fixture.root, fixture.source.as_os_str()).is_err()
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn times_out_and_terminates_commands_that_keep_their_pipes_open() {
        let started = Instant::now();
        let error = bounded_output(
            Command::new("sh").args(["-c", "sleep 20"]),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn drains_large_stdout_and_stderr_without_pipe_deadlock() {
        let result = bounded_output(
            Command::new("sh").args([
                "-c",
                "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
            ]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout.len(), 262144);
        assert_eq!(result.stderr.len(), 262144);
    }
}
