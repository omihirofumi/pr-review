use std::{
    fmt, fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    context::{FileContext, FullFile},
    diff::validate_comment,
    model::{
        ChangedFile, Check, CheckState, ConversationEntry, DraftComment, PrActivity, PrId,
        PrOverview, PrSummary, PullRequest, ReviewComment, ReviewData, ReviewEvent,
    },
};

const GH_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct CachedContext {
    identity: String,
    source: FileContext,
}

pub fn open_in_browser(id: &PrId) -> Result<()> {
    validate_id(id)?;
    gh(
        &[
            "pr",
            "view",
            &id.number.to_string(),
            "--repo",
            &id.repo,
            "--web",
        ],
        None,
    )?;
    Ok(())
}

pub fn load_file_context(
    pr: &PullRequest,
    file: &ChangedFile,
    cache_root: &Path,
) -> Result<FileContext> {
    validate_id(&pr.id)?;
    validate_sha(&pr.base_sha)?;
    validate_sha(&pr.head_sha)?;
    let identity = serde_json::to_string(
        &json!({"repo": pr.id.repo, "base": pr.base_sha, "head": pr.head_sha, "file": file}),
    )?;
    let path = cache_root
        .join(&pr.id.repo)
        .join(crate::cache::file_name(&identity));
    if let Ok(bytes) = fs::read(&path)
        && bytes.len() <= MAX_CONTEXT_BYTES * 4
        && let Ok(entry) = serde_json::from_slice::<CachedContext>(&bytes)
        && entry.identity == identity
        && FullFile::new(file, &entry.source).is_ok()
    {
        return Ok(entry.source);
    }
    let source = thread::scope(|scope| -> Result<FileContext> {
        let base = scope.spawn(|| {
            if file.status == "added" {
                Ok(String::new())
            } else {
                file_at(
                    &pr.id.repo,
                    file.previous_filename.as_deref().unwrap_or(&file.filename),
                    &pr.base_sha,
                )
            }
        });
        let head = scope.spawn(|| {
            if file.status == "removed" {
                Ok(String::new())
            } else {
                file_at(&pr.id.repo, &file.filename, &pr.head_sha)
            }
        });
        let base = base
            .join()
            .map_err(|_| anyhow::anyhow!("Base file worker failed"))?;
        let head = head
            .join()
            .map_err(|_| anyhow::anyhow!("Head file worker failed"))?;
        Ok(FileContext {
            base: base?,
            head: head?,
        })
    })?;
    FullFile::new(file, &source)?;
    if let Some(parent) = path.parent()
        && fs::create_dir_all(parent).is_ok()
        && let Ok(mut temporary) = tempfile::NamedTempFile::new_in(parent)
        && serde_json::to_writer(
            &mut temporary,
            &CachedContext {
                identity,
                source: source.clone(),
            },
        )
        .is_ok()
        && temporary.as_file().sync_all().is_ok()
    {
        let _ = temporary.persist(path);
    }
    Ok(source)
}

pub fn file_at(repo: &str, path: &str, revision: &str) -> Result<String> {
    ensure!(
        !path.is_empty()
            && !path.contains('\0')
            && path
                .split('/')
                .all(|part| !part.is_empty() && !matches!(part, "." | "..")),
        "Invalid source file path"
    );
    let encoded = path
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect::<String>();
    let endpoint = format!("repos/{repo}/contents/{encoded}?ref={revision}");
    let mut value: Value = gh_json(&[
        "api",
        "--method",
        "GET",
        "-H",
        "Accept: application/vnd.github.object+json",
        &endpoint,
    ])?;
    ensure!(
        value["type"] == "file" && value.get("submodule_git_url").is_none_or(Value::is_null),
        "Only regular text files can be expanded"
    );
    let size = value["size"]
        .as_u64()
        .context("GitHub did not return a file size")?;
    ensure!(
        size <= MAX_CONTEXT_BYTES as u64,
        "File exceeds the 2 MiB context limit; open it in Helix"
    );
    if value["encoding"] == "none" {
        let sha = value["sha"].as_str().context("Missing file blob SHA")?;
        validate_sha(sha)?;
        value = gh_json(&[
            "api",
            "--method",
            "GET",
            &format!("repos/{repo}/git/blobs/{sha}"),
        ])?;
    }
    ensure!(
        value["encoding"] == "base64",
        "GitHub did not return readable file content"
    );
    let content = value["content"]
        .as_str()
        .context("GitHub did not return file content")?;
    ensure!(
        content.len() <= MAX_CONTEXT_BYTES * 2,
        "Encoded file content exceeds the context limit"
    );
    let encoded = content
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("GitHub returned invalid file encoding")?;
    ensure!(
        bytes.len() == size as usize && !bytes.contains(&0),
        "Binary or incomplete file content cannot be expanded"
    );
    String::from_utf8(bytes).context("The file is not UTF-8 text; open it in Helix")
}

#[derive(Debug)]
pub struct SubmissionUncertain(anyhow::Error);

impl fmt::Display for SubmissionUncertain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GitHub may have accepted this review. Check the PR before retrying; the submission result is uncertain: {:#}",
            self.0
        )
    }
}

impl std::error::Error for SubmissionUncertain {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub fn is_submission_uncertain(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SubmissionUncertain>().is_some()
}

pub fn resolve_repo(repository: Option<&str>) -> Result<String> {
    if let Some(repository) = repository {
        validate_repo(repository)?;
        return Ok(repository.to_owned());
    }
    let args = ["repo", "view", "--json", "nameWithOwner"];
    let bytes = match gh(&args, None) {
        Ok(bytes) => bytes,
        Err(original) => {
            let root = Command::new("jj")
                .args(["--no-pager", "--color=never", "git", "root"])
                .stdin(Stdio::null())
                .output();
            let bytes = root
                .ok()
                .filter(|result| result.status.success())
                .and_then(|result| String::from_utf8(result.stdout).ok())
                .and_then(|root| gh_in(&args, None, Some(Path::new(root.trim()))).ok());
            bytes.with_context(|| {
                format!(
                    "Cannot resolve a GitHub repository from this directory; pass --repo owner/repo: {original:#}"
                )
            })?
        }
    };
    let value: Value = serde_json::from_slice(&bytes)
        .context("GitHub CLI returned invalid repository information")?;
    let repo = value["nameWithOwner"]
        .as_str()
        .context("GitHub CLI did not return a repository; pass --repo owner/repo")?;
    validate_repo(repo)?;
    Ok(repo.to_owned())
}

pub fn parse_target(target: &str, repo: Option<&str>) -> Result<PrId> {
    if let Some(path) = target.strip_prefix("https://github.com/") {
        let path = path
            .split(['?', '#'])
            .next()
            .unwrap_or(path)
            .trim_end_matches('/');
        let pieces: Vec<_> = path.split('/').collect();
        ensure!(
            pieces.len() == 4 && pieces[2] == "pull",
            "Expected a GitHub PR URL: https://github.com/owner/repo/pull/123"
        );
        let url_repo = format!("{}/{}", pieces[0], pieces[1]);
        validate_repo(&url_repo)?;
        if let Some(repo) = repo {
            ensure!(
                repo.eq_ignore_ascii_case(&url_repo),
                "PR URL repository {url_repo} does not match --repo {repo}"
            );
        }
        return Ok(PrId {
            repo: url_repo,
            number: parse_number(pieces[3])?,
        });
    }

    let number = parse_number(target.strip_prefix('#').unwrap_or(target))?;
    let repo = resolve_repo(repo)?;
    Ok(PrId { repo, number })
}

pub fn list_prs(repo: Option<&str>, search: Option<&str>) -> Result<Vec<PrSummary>> {
    let repo = resolve_repo(repo)?;
    let mut args = vec![
        "pr",
        "list",
        "--repo",
        &repo,
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,title,body,author,isDraft,url",
    ];
    if let Some(search) = search.filter(|search| !search.trim().is_empty()) {
        args.extend(["--search", search]);
    }
    let prs: Vec<SearchPr> = gh_json(&args)?;
    prs.into_iter()
        .map(|pr| {
            let id = parse_target(&pr.url, Some(&repo))?;
            ensure!(
                id.number == pr.number,
                "GitHub returned an inconsistent PR number"
            );
            Ok(PrSummary {
                body: pr.body.unwrap_or_default(),
                id,
                title: pr.title,
                author: login(pr.author),
                draft: pr.is_draft,
            })
        })
        .collect()
}

pub fn load_overview(id: &PrId) -> Result<PrOverview> {
    let pr = fetch_pr(id)?;
    validate_sha(&pr.head.sha)?;
    validate_sha(&pr.base.sha)?;
    Ok(PrOverview {
        id: id.clone(),
        title: pr.title,
        body: pr.body.unwrap_or_default(),
        url: pr.html_url,
        author: login(pr.user),
        head_sha: pr.head.sha,
        base_tip_sha: pr.base.sha,
        draft: pr.draft,
        state: pr.state,
        changed_files: pr.changed_files,
        additions: pr.additions,
        deletions: pr.deletions,
    })
}

pub fn load_diff(overview: &PrOverview) -> Result<ReviewData> {
    validate_overview(overview)?;
    ensure!(
        overview.changed_files <= 3000,
        "This PR has {} files; GitHub's files API is limited to 3000",
        overview.changed_files
    );
    let (base_sha, files) = thread::scope(|scope| -> Result<(String, Vec<ChangedFile>)> {
        let base =
            scope.spawn(|| merge_base(&overview.id, &overview.base_tip_sha, &overview.head_sha));
        let files = scope.spawn(|| {
            paginated(&format!(
                "repos/{}/pulls/{}/files?per_page=100",
                overview.id.repo, overview.id.number
            ))
        });
        let base = base
            .join()
            .map_err(|_| anyhow::anyhow!("Diff base worker failed"));
        let files = files
            .join()
            .map_err(|_| anyhow::anyhow!("Changed files worker failed"));
        Ok((base??, files??))
    })?;
    verify_overview(overview)?;
    ensure!(
        files.len() as u64 == overview.changed_files,
        "GitHub returned {} of {} changed files; reload the PR",
        files.len(),
        overview.changed_files
    );
    Ok(ReviewData {
        source: crate::model::ReviewSource::GitHub,
        pr: PullRequest {
            id: overview.id.clone(),
            title: overview.title.clone(),
            body: overview.body.clone(),
            url: overview.url.clone(),
            author: overview.author.clone(),
            head_sha: overview.head_sha.clone(),
            base_sha,
            draft: overview.draft,
        },
        files,
        comments: Vec::new(),
    })
}

pub fn load_comments(overview: &PrOverview) -> Result<Vec<ReviewComment>> {
    validate_overview(overview)?;
    let comments: Vec<ApiComment> = paginated(&format!(
        "repos/{}/pulls/{}/comments?per_page=100",
        overview.id.repo, overview.id.number
    ))?;
    verify_overview(overview)?;
    Ok(comments.into_iter().map(review_comment).collect())
}

fn validate_overview(overview: &PrOverview) -> Result<()> {
    validate_id(&overview.id)?;
    validate_sha(&overview.head_sha)?;
    validate_sha(&overview.base_tip_sha)
}

fn verify_overview(overview: &PrOverview) -> Result<()> {
    let current = fetch_pr(&overview.id)?;
    ensure!(
        current.head.sha == overview.head_sha && current.base.sha == overview.base_tip_sha,
        "PR changed since the overview loaded; refresh before loading its diff or comments"
    );
    Ok(())
}

fn review_comment(comment: ApiComment) -> ReviewComment {
    ReviewComment {
        id: comment.id,
        path: comment.path,
        body: comment.body,
        author: login(comment.user),
        line: comment.line,
        original_line: comment.original_line,
        side: comment.side,
        start_line: comment.start_line,
        start_side: comment.start_side,
        in_reply_to_id: comment.in_reply_to_id,
    }
}

pub fn load_review(id: &PrId) -> Result<ReviewData> {
    validate_id(id)?;
    let initial = fetch_pr(id)?;
    ensure!(
        initial.changed_files <= 3000,
        "This PR has {} files; GitHub's files API is limited to 3000",
        initial.changed_files
    );
    let base_sha = merge_base(id, &initial.base.sha, &initial.head.sha)?;
    let files: Vec<ChangedFile> = paginated(&format!(
        "repos/{}/pulls/{}/files?per_page=100",
        id.repo, id.number
    ))?;
    ensure!(
        files.len() as u64 == initial.changed_files,
        "GitHub returned {} of {} changed files; reload the PR",
        files.len(),
        initial.changed_files
    );
    let comments: Vec<ApiComment> = paginated(&format!(
        "repos/{}/pulls/{}/comments?per_page=100",
        id.repo, id.number
    ))?;
    let final_pr = fetch_pr(id)?;
    ensure!(
        final_pr.head.sha == initial.head.sha && final_pr.base.sha == initial.base.sha,
        "PR changed while loading; reload to review its current diff"
    );
    Ok(ReviewData {
        source: crate::model::ReviewSource::GitHub,
        pr: PullRequest {
            id: id.clone(),
            title: initial.title,
            body: initial.body.unwrap_or_default(),
            url: initial.html_url,
            author: login(initial.user),
            head_sha: initial.head.sha,
            base_sha,
            draft: initial.draft,
        },
        files,
        comments: comments.into_iter().map(review_comment).collect(),
    })
}

pub fn load_activity(id: &PrId) -> Result<PrActivity> {
    validate_id(id)?;
    let number = id.number.to_string();
    let view: ApiActivity = gh_json(&[
        "pr",
        "view",
        &number,
        "--repo",
        &id.repo,
        "--json",
        "labels,statusCheckRollup,comments,reviews",
    ])?;
    Ok(activity(view))
}

pub fn conversation_text(id: &PrId) -> Result<String> {
    let activity = load_activity(id)?;
    if activity.conversation.is_empty() {
        return Ok("No conversation comments or reviews found.".into());
    }
    Ok(activity
        .conversation
        .iter()
        .map(|entry| {
            format!(
                "@{} | {} | {}\n{}",
                entry.author,
                entry.action,
                entry.time(),
                entry.body
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n"))
}

fn activity(view: ApiActivity) -> PrActivity {
    let mut latest = Vec::<(String, String, Check)>::new();
    for item in view.status_check_rollup {
        let (key, started, check) = match item {
            ApiCheck::CheckRun {
                name,
                workflow_name,
                status,
                conclusion,
                details_url,
                started_at,
            } => {
                let conclusion = conclusion.unwrap_or_default();
                let (state, detail) = if status != "COMPLETED" {
                    (CheckState::Pending, status.to_lowercase())
                } else {
                    (check_run_state(&conclusion), conclusion.to_lowercase())
                };
                let display = match workflow_name.filter(|workflow| !workflow.is_empty()) {
                    Some(workflow) => format!("{workflow} / {name}"),
                    None => name,
                };
                (
                    display.clone(),
                    started_at.unwrap_or_default(),
                    Check {
                        name: display,
                        state,
                        detail: detail.replace('_', " "),
                        url: details_url.filter(|url| !url.is_empty()),
                    },
                )
            }
            ApiCheck::StatusContext {
                context,
                state,
                target_url,
                started_at,
            } => (
                context.clone(),
                started_at.unwrap_or_default(),
                Check {
                    name: context,
                    state: match state.as_str() {
                        "SUCCESS" => CheckState::Passed,
                        "PENDING" | "EXPECTED" => CheckState::Pending,
                        _ => CheckState::Failed,
                    },
                    detail: state.to_lowercase(),
                    url: target_url.filter(|url| !url.is_empty()),
                },
            ),
            ApiCheck::Unknown => continue,
        };
        match latest.iter_mut().find(|(existing, ..)| *existing == key) {
            Some(entry) if entry.1 <= started => *entry = (key, started, check),
            Some(_) => {}
            None => latest.push((key, started, check)),
        }
    }
    let mut checks = latest
        .into_iter()
        .map(|(.., check)| check)
        .collect::<Vec<_>>();
    checks.sort_by(|left, right| (left.state, &left.name).cmp(&(right.state, &right.name)));
    let mut conversation = view
        .comments
        .into_iter()
        .filter(|comment| !comment.is_minimized)
        .map(|comment| ConversationEntry {
            author: login(comment.author),
            action: "commented".into(),
            created_at: comment.created_at,
            body: comment.body,
        })
        .chain(
            view.reviews
                .into_iter()
                .filter(|review| review.state != "COMMENTED" || !review.body.trim().is_empty())
                .map(|review| ConversationEntry {
                    author: login(review.author),
                    action: match review.state.as_str() {
                        "APPROVED" => "approved".into(),
                        "CHANGES_REQUESTED" => "requested changes".into(),
                        "COMMENTED" => "reviewed".into(),
                        "DISMISSED" => "review dismissed".into(),
                        other => other.to_lowercase().replace('_', " "),
                    },
                    created_at: review.submitted_at.unwrap_or_default(),
                    body: review.body,
                }),
        )
        .collect::<Vec<_>>();
    conversation.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    PrActivity {
        labels: view.labels.into_iter().map(|label| label.name).collect(),
        checks,
        conversation,
    }
}

fn check_run_state(conclusion: &str) -> CheckState {
    match conclusion {
        "SUCCESS" => CheckState::Passed,
        "NEUTRAL" | "SKIPPED" | "CANCELLED" => CheckState::Skipped,
        _ => CheckState::Failed,
    }
}

pub fn submit_review(
    data: &ReviewData,
    drafts: &[DraftComment],
    body: &str,
    event: ReviewEvent,
) -> Result<String> {
    ensure!(
        !data.source.is_local(),
        "Local reviews cannot be submitted to GitHub"
    );
    let payload = review_payload(data, drafts, body, event)?;
    let current = fetch_pr(&data.pr.id)?;
    ensure!(current.state == "open", "This PR is no longer open");
    ensure!(
        current.head.sha == data.pr.head_sha,
        "PR head changed since loading; reload before submitting (drafts are preserved)"
    );
    ensure!(
        merge_base(&data.pr.id, &current.base.sha, &current.head.sha)? == data.pr.base_sha,
        "PR diff base changed since loading; reload before submitting (drafts are preserved)"
    );
    let path = format!(
        "repos/{}/pulls/{}/reviews",
        data.pr.id.repo, data.pr.id.number
    );
    let bytes = gh(
        &["api", "--method", "POST", &path, "--input", "-"],
        Some(serde_json::to_vec(&payload)?),
    )
    .map_err(|error| anyhow::Error::new(SubmissionUncertain(error)))?;
    let response: Value = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::Error::new(SubmissionUncertain(
            anyhow::Error::new(error).context("Invalid review response from GitHub"),
        ))
    })?;
    Ok(response["html_url"]
        .as_str()
        .unwrap_or(&data.pr.url)
        .to_owned())
}

fn review_payload(
    data: &ReviewData,
    drafts: &[DraftComment],
    body: &str,
    event: ReviewEvent,
) -> Result<Value> {
    validate_id(&data.pr.id)?;
    validate_sha(&data.pr.head_sha)?;
    validate_sha(&data.pr.base_sha)?;
    ensure!(
        event == ReviewEvent::Approve || !body.trim().is_empty(),
        "A review summary is required for comments and change requests"
    );
    for draft in drafts {
        ensure!(!draft.body.trim().is_empty(), "An inline comment is empty");
        let file = data
            .files
            .iter()
            .find(|file| file.filename == draft.path)
            .with_context(|| format!("Comment file is not in this diff: {}", draft.path))?;
        ensure!(
            validate_comment(file, draft),
            "Comment target is not in the displayed diff: {}:{} {:?}",
            draft.path,
            draft.line_label(),
            draft.side
        );
    }
    Ok(json!({
        "commit_id": data.pr.head_sha,
        "event": event.as_str(),
        "body": body,
        "comments": drafts,
    }))
}

fn fetch_pr(id: &PrId) -> Result<ApiPr> {
    validate_id(id)?;
    let path = format!("repos/{}/pulls/{}", id.repo, id.number);
    gh_json(&["api", "--method", "GET", &path])
}

fn merge_base(id: &PrId, base: &str, head: &str) -> Result<String> {
    validate_sha(base)?;
    validate_sha(head)?;
    let path = format!("repos/{}/compare/{base}...{head}?per_page=1", id.repo);
    let response: Value = gh_json(&["api", "--method", "GET", &path])?;
    let sha = response["merge_base_commit"]["sha"]
        .as_str()
        .context("GitHub did not return the PR's diff base")?;
    validate_sha(sha)?;
    Ok(sha.to_owned())
}

fn paginated<T: DeserializeOwned>(path: &str) -> Result<Vec<T>> {
    let pages: Vec<Vec<T>> = gh_json(&["api", "--method", "GET", "--paginate", "--slurp", path])?;
    Ok(pages.into_iter().flatten().collect())
}

fn gh_json<T: DeserializeOwned>(args: &[&str]) -> Result<T> {
    serde_json::from_slice(&gh(args, None)?)
        .with_context(|| format!("GitHub CLI returned invalid JSON for gh {}", args.join(" ")))
}

fn gh(args: &[&str], input: Option<Vec<u8>>) -> Result<Vec<u8>> {
    gh_in(args, input, None)
}

fn gh_in(args: &[&str], input: Option<Vec<u8>>, directory: Option<&Path>) -> Result<Vec<u8>> {
    let mut command = Command::new("gh");
    command
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_HOST", "github.com")
        .env("GH_PAGER", "cat")
        .env_remove("GH_REPO")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let mut child = command
        .spawn()
        .context("Could not run GitHub CLI; install gh and run gh auth login")?;
    let mut stdout = child.stdout.take().context("Missing gh stdout")?;
    let mut stderr = child.stderr.take().context("Missing gh stderr")?;
    let stdout_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let input_thread = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        thread::spawn(move || stdin.write_all(&bytes))
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().context("Could not wait for GitHub CLI")? {
            break status;
        }
        if started.elapsed() >= GH_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "GitHub CLI timed out after {} seconds",
                GH_TIMEOUT.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    let output = stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("GitHub output reader failed"))??;
    let error = stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("GitHub error reader failed"))??;
    ensure!(
        status.success(),
        "gh {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&error).trim()
    );
    if let Some(input) = input_thread {
        input
            .join()
            .map_err(|_| anyhow::anyhow!("GitHub input writer failed"))??;
    }
    Ok(output)
}

fn parse_number(number: &str) -> Result<u64> {
    ensure!(
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()),
        "Expected a positive PR number or https://github.com/owner/repo/pull/123"
    );
    let number: u64 = number.parse().context("PR number is too large")?;
    ensure!(number > 0, "PR number must be positive");
    Ok(number)
}

fn validate_repo(repo: &str) -> Result<()> {
    let parts: Vec<_> = repo.split('/').collect();
    ensure!(
        parts.len() == 2
            && !parts[0].is_empty()
            && !parts[1].is_empty()
            && parts[0]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && parts[1]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            && ![".", ".."].contains(&parts[1]),
        "Expected a repository in owner/repo format"
    );
    Ok(())
}

fn validate_id(id: &PrId) -> Result<()> {
    validate_repo(&id.repo)?;
    ensure!(id.number > 0, "PR number must be positive");
    Ok(())
}

fn validate_sha(sha: &str) -> Result<()> {
    ensure!(
        [40, 64].contains(&sha.len()) && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "GitHub returned an invalid commit SHA"
    );
    Ok(())
}

fn login(user: Option<ApiUser>) -> String {
    user.map(|user| user.login)
        .unwrap_or_else(|| "[deleted]".into())
}

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Deserialize)]
struct ApiRef {
    sha: String,
}

#[derive(Deserialize)]
struct ApiPr {
    title: String,
    body: Option<String>,
    html_url: String,
    user: Option<ApiUser>,
    head: ApiRef,
    base: ApiRef,
    draft: bool,
    changed_files: u64,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    state: String,
}

#[derive(Deserialize)]
struct ApiComment {
    id: u64,
    path: String,
    body: String,
    user: Option<ApiUser>,
    line: Option<u32>,
    original_line: Option<u32>,
    side: String,
    #[serde(default)]
    start_line: Option<u32>,
    #[serde(default)]
    start_side: Option<String>,
    in_reply_to_id: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiActivity {
    #[serde(default)]
    labels: Vec<ApiLabel>,
    #[serde(default)]
    status_check_rollup: Vec<ApiCheck>,
    #[serde(default)]
    comments: Vec<ApiConversationComment>,
    #[serde(default)]
    reviews: Vec<ApiConversationReview>,
}

#[derive(Deserialize)]
struct ApiLabel {
    name: String,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum ApiCheck {
    #[serde(rename_all = "camelCase")]
    CheckRun {
        name: String,
        workflow_name: Option<String>,
        #[serde(default)]
        status: String,
        conclusion: Option<String>,
        details_url: Option<String>,
        started_at: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    StatusContext {
        context: String,
        #[serde(default)]
        state: String,
        target_url: Option<String>,
        started_at: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiConversationComment {
    author: Option<ApiUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    is_minimized: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiConversationReview {
    author: Option<ApiUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct SearchPr {
    body: Option<String>,
    number: u64,
    title: String,
    author: Option<ApiUser>,
    #[serde(rename = "isDraft")]
    is_draft: bool,
    url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Side;

    #[test]
    fn activity_keeps_latest_check_runs_and_meaningful_conversation() {
        let view: ApiActivity = serde_json::from_value(json!({
            "labels": [{"name": "api"}],
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "build", "workflowName": "CI", "status": "COMPLETED", "conclusion": "FAILURE", "startedAt": "2026-01-01T00:00:00Z"},
                {"__typename": "CheckRun", "name": "build", "workflowName": "CI", "status": "COMPLETED", "conclusion": "SUCCESS", "startedAt": "2026-01-02T00:00:00Z"},
                {"__typename": "CheckRun", "name": "lint", "workflowName": "CI", "status": "IN_PROGRESS", "conclusion": null},
                {"__typename": "StatusContext", "context": "deploy", "state": "ERROR", "targetUrl": ""},
                {"__typename": "FutureCheckKind"}
            ],
            "comments": [
                {"author": {"login": "bot"}, "body": "hidden", "createdAt": "2026-01-01T00:00:00Z", "isMinimized": true},
                {"author": {"login": "alice"}, "body": "Question", "createdAt": "2026-01-03T00:00:00Z", "isMinimized": false}
            ],
            "reviews": [
                {"author": {"login": "bob"}, "body": "", "state": "COMMENTED", "submittedAt": "2026-01-02T00:00:00Z"},
                {"author": {"login": "carol"}, "body": "", "state": "APPROVED", "submittedAt": "2026-01-04T00:00:00Z"},
                {"author": {"login": "dan"}, "body": "Summary", "state": "COMMENTED", "submittedAt": "2026-01-02T12:00:00Z"}
            ]
        }))
        .unwrap();
        let activity = activity(view);
        assert_eq!(activity.labels, ["api"]);
        assert_eq!(
            activity
                .checks
                .iter()
                .map(|check| (check.name.as_str(), check.state, check.url.is_some()))
                .collect::<Vec<_>>(),
            [
                ("deploy", CheckState::Failed, false),
                ("CI / lint", CheckState::Pending, false),
                ("CI / build", CheckState::Passed, false),
            ]
        );
        assert_eq!(
            activity
                .conversation
                .iter()
                .map(|entry| (entry.author.as_str(), entry.action.as_str()))
                .collect::<Vec<_>>(),
            [
                ("dan", "reviewed"),
                ("alice", "commented"),
                ("carol", "approved")
            ]
        );
    }

    #[test]
    fn parses_urls_numbers_and_repository_conflicts() {
        assert_eq!(
            parse_target(
                "https://github.com/owner/repo/pull/42/?diff=split#discussion",
                None
            )
            .unwrap(),
            PrId {
                repo: "owner/repo".into(),
                number: 42
            }
        );
        assert_eq!(parse_target("#12", Some("owner/repo")).unwrap().number, 12);
        assert!(parse_target("https://github.com/owner/repo/pull/42", Some("other/repo")).is_err());
    }

    #[test]
    fn rejects_non_pr_urls_and_unsafe_repository_components() {
        for target in [
            "0",
            "-1",
            "https://evil.test/owner/repo/pull/42",
            "https://github.com/owner/repo/issues/42",
            "https://github.com/owner/repo/pull/42/files",
        ] {
            assert!(
                parse_target(target, Some("owner/repo")).is_err(),
                "accepted {target}"
            );
        }
        for repo in [
            "../repo",
            "owner/..",
            "owner/repo/extra",
            "owner/repo?x=1",
            "--help",
        ] {
            assert!(parse_target("42", Some(repo)).is_err(), "accepted {repo}");
        }
    }

    fn data() -> ReviewData {
        ReviewData {
            source: crate::model::ReviewSource::GitHub,
            pr: PullRequest {
                id: PrId {
                    repo: "owner/repo".into(),
                    number: 42,
                },
                title: "Title".into(),
                body: String::new(),
                url: "https://github.com/owner/repo/pull/42".into(),
                author: "author".into(),
                head_sha: "a".repeat(40),
                base_sha: "b".repeat(40),
                draft: false,
            },
            files: vec![ChangedFile {
                filename: "renamed.rs".into(),
                previous_filename: Some("old.rs".into()),
                status: "renamed".into(),
                additions: 1,
                deletions: 1,
                patch: Some("@@ -7,2 +7,2 @@\n-old\n+new\n context".into()),
            }],
            comments: Vec::new(),
        }
    }

    #[test]
    fn pins_commit_and_preserves_deleted_side_coordinates() {
        let draft = DraftComment {
            start_line: None,
            start_side: None,
            path: "renamed.rs".into(),
            line: 7,
            side: Side::Left,
            body: "Explain why".into(),
        };
        let payload = review_payload(
            &data(),
            &[draft],
            "Please revise",
            ReviewEvent::RequestChanges,
        )
        .unwrap();
        assert_eq!(payload["commit_id"], "a".repeat(40));
        assert_eq!(payload["event"], "REQUEST_CHANGES");
        assert_eq!(payload["comments"][0]["path"], "renamed.rs");
        assert_eq!(payload["comments"][0]["side"], "LEFT");
        assert_eq!(payload["comments"][0]["line"], 7);
    }

    #[test]
    fn rejects_comment_targets_outside_the_reviewed_patch() {
        let invalid = DraftComment {
            start_line: None,
            start_side: None,
            path: "renamed.rs".into(),
            line: 100,
            side: Side::Right,
            body: "Comment".into(),
        };
        assert!(review_payload(&data(), &[invalid], "Summary", ReviewEvent::Comment).is_err());
        assert!(review_payload(&data(), &[], " ", ReviewEvent::RequestChanges).is_err());
        assert!(review_payload(&data(), &[], "", ReviewEvent::Approve).is_ok());
    }

    #[test]
    fn submission_uncertainty_remains_detectable_through_context() {
        let uncertain =
            anyhow::Error::new(SubmissionUncertain(anyhow::anyhow!("transport failed")))
                .context("Review submission failed");
        assert!(is_submission_uncertain(&uncertain));
        assert!(format!("{uncertain:#}").contains("may have accepted"));
        assert!(!is_submission_uncertain(&anyhow::anyhow!(
            "PR head changed"
        )));
    }

    #[test]
    fn submission_preserves_multiline_coordinates_without_changing_single_line_shape() {
        let draft = DraftComment {
            path: "renamed.rs".into(),
            line: 8,
            side: Side::Left,
            start_line: Some(7),
            start_side: Some(Side::Left),
            body: "Preserve this invariant across the whole block.".into(),
        };
        let payload =
            review_payload(&data(), &[draft], "Review summary", ReviewEvent::Comment).unwrap();
        assert_eq!(
            payload["comments"][0],
            json!({
                "path": "renamed.rs", "line": 8, "side": "LEFT",
                "start_line": 7, "start_side": "LEFT",
                "body": "Preserve this invariant across the whole block."
            })
        );
        assert_eq!(payload["commit_id"], "a".repeat(40));
    }

    #[test]
    fn api_comments_accept_legacy_and_multiline_coordinates() {
        let mut value = json!({
            "id": 1, "path": "a.rs", "body": "Comment", "user": {"login": "reviewer"},
            "line": 21, "original_line": 20, "side": "RIGHT", "in_reply_to_id": null
        });
        let single: ApiComment = serde_json::from_value(value.clone()).unwrap();
        assert!(single.start_line.is_none());
        assert!(single.start_side.is_none());
        value["start_line"] = json!(19);
        value["start_side"] = json!("RIGHT");
        let ranged: ApiComment = serde_json::from_value(value).unwrap();
        assert_eq!(ranged.start_line, Some(19));
        assert_eq!(ranged.start_side.as_deref(), Some("RIGHT"));
    }
}
