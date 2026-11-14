# gh-preview

> Fully vibe-coded and intended for personal use.

A standalone Rust + Ratatui app for reviewing GitHub pull requests with Jujutsu and Helix.

Run it inside a repository to browse that repository's PRs, read diffs and inline threads, prepare an isolated checkout, jump into Helix, and submit a batch of inline comments from the terminal. `pr-review` remains available as an alias.

## Install

### Homebrew

```sh
brew tap omihirofumi/tap
brew install omihirofumi/tap/gh-preview
```

The formula builds the tagged release from source and installs GitHub CLI, Jujutsu, Git, and Helix as dependencies. Update it with `brew update` followed by `brew upgrade omihirofumi/tap/gh-preview`.

If you previously installed this project with Cargo, run `cargo uninstall pr-review` after the Homebrew installation succeeds. This removes the older copies from Cargo's bin directory so `gh-preview` resolves to the Homebrew installation. Check with `command -v gh-preview`.

### Cargo

Install Rust, [GitHub CLI](https://cli.github.com/), [Jujutsu](https://docs.jj-vcs.dev/latest/install-and-setup/), Git, and [Helix](https://docs.helix-editor.com/install.html). Make `gh`, `jj`, `git`, and `hx` available on `PATH`.

From the source checkout:

```sh
gh auth login
cargo install --path . --locked
```

## Quick start

Run the app from the repository you want to review, not necessarily this project's checkout:

```sh
cd /path/to/your/repository
gh-preview
```

Select a PR with `j` / `k` and press `o`. If you already know its number, open it directly with `gh-preview 123`. Subdirectories work too; the app detects the containing repository.

Once the diff is open:

1. Keep pressing `j` to read downward and continue into the next file. `k` goes backward. Use `gg` / `ge` to jump to the current file's top / bottom.
2. Press `gf` or `o` to open the selected source line in Helix. Quit Helix to resume the review at the same position.
3. Press `c` to comment on one line. For three lines, press `xxx`, then `c`; for a variable range, press `v`, move with `j` / `k`, then press `c`. In the diff view, `Esc` or `;` clears a selection. Ranges cannot combine removed and added lines or span separate hunks.
4. Write the comment and press `Ctrl-s` to save a **local draft**. `Ctrl-h` deletes backward. Press `D` to inspect or edit saved drafts.
5. To publish the review, press `s`, write its summary, and use `Tab` to choose comment, approve, or request changes. Press `Ctrl-s` to preview, then `Enter` to send it to GitHub. `Esc` cancels the preview.

`Ctrl-s` inside the comment editor does not publish anything. The final preview's `Enter` is the publishing action.

Press `t` to hide/show inline comments, `T` to read complete threads, and `V` to read published review summaries. Press `#` to open another PR by number, `?` for the keyboard guide, or `q` to quit. After installing an update, restart the app to use the new version.

To learn the controls without contacting GitHub, run `gh-preview --demo`. The demo allows local drafts and review previews; source checkout and Helix opening require a real PR.

## Command options

```sh
gh-preview                                      # Open PRs in the current repository
gh-preview 123                                  # PR #123 in the current repository
gh-preview --search 'author:@me'                 # Filter PRs in the current repository
gh-preview --repo owner/repo                     # Explicit repository override
gh-preview --repo owner/repo 123
gh-preview https://github.com/owner/repo/pull/123
gh-preview --demo                               # Offline example; never posts
```

A repository is detected from the current directory, including subdirectories. A PR URL supplies its own repository. The inbox, refresh, search, and direct number entry stay scoped to that selected repository; there is no account-wide review queue. Outside a repository, use `--repo` or a PR URL.

The inbox shows up to 100 open PRs. `/` filters the loaded inbox locally; `--search` queries GitHub within the selected repository. Press `#` from the inbox or review screen to open a PR by number without restarting.

## Review controls

Comments on deleted lines target the LEFT side; added and context-only ranges target the RIGHT side. Multiline comments must stay within one complete hunk on a single side; a selection containing both deleted and added lines cannot be submitted as one range. Both the displayed revision and comment coordinates are checked before submission. A changed PR head or diff base requires a refresh with `r`.

`T` opens the full inline thread list, including outdated comments and comments outside the displayed patch. `V` shows published review summaries. `?` shows the keyboard guide. Long operation messages can be read with `!`.

Navigation and selection follow Helix's line-oriented workflow: `gg` goes to the top of the current file, `ge` to its end, and `gf` opens the selected source location in Helix. `v` toggles selection-extension mode; while active, movement extends the range and stays in the current file. Toggling `v` off retains the range until the next normal movement. `x` selects the current line, and repeated `x` presses extend downward. `X` selects whole-line bounds without adding another line. `;` or `Esc` clears the range. Starting a selection on a hunk header moves to a nearby numbered source line.

| Key | Action |
| --- | --- |
| `Tab`, `h`, `l` | Focus files or diff |
| `j` / `k`, up / down | Move through lines and continue into adjacent files |
| `n` / `p` | Next / previous file |
| `]` / `[` | Next / previous hunk |
| `gg` / `ge` | Top / end of current file, or first / last item in lists |
| `gf` | Open selected source location in Helix |
| `G`, Home / End | Legacy file or list boundary shortcuts |
| `v` | Toggle selection-extension mode |
| `x` / `X` | Select/extend whole lines downward / select whole-line bounds |
| `;` | Collapse the selection at the cursor |
| `Ctrl-d` / `Ctrl-u`, PageDown / PageUp | Move through the diff |
| left / right | Scroll long diff lines horizontally |
| `b` | PR description |
| `t` | Toggle inline comments, replies, and local drafts |
| `T` / `V` | Full inline thread list / published reviews |
| `#` | Open a PR by number in the selected repository |
| `o` | Open the selected PR, source file in Helix, or saved draft |
| `w` / `e` | Prepare checkout / open source in Helix |
| `c` / `D` | Compose comment / manage saved drafts |
| `s` | Preview and submit review |
| `u` | Recover a submission with an unknown result |
| `!` / `?` | Operation details / keyboard guide |
| `r` | Refresh inbox or current PR |
| `Esc` / `q` | Clear a range, otherwise return to inbox / quit |

The Hunk Review-inspired layout uses a compact file sidebar, separate hunk headers, and shaded addition/deletion rows. The selected source line is highlighted across its full width, with distinct selection colors for added and deleted lines. Comment counts remain visible when the inline threads are hidden. Toggling threads preserves the selected source line, including its comment and Helix coordinates. Long inline cards show a preview; use `T` to read the full published thread, or `D` and `o` to open the full draft.

The terminal needs at least 48 columns and 12 rows; 100 columns or more gives the diff more space. Paste is supported in comment, summary, and PR-number editors. Arrow keys, Home, End, Backspace, `Ctrl-h` (backspace), and Delete edit text.

## Local workspaces and drafts

The application creates its own Jujutsu repositories and revision-specific review workspaces in the platform's local application-data directory. It does not switch branches or change files in the repository you launched it from. Override the data location with `--workspace-root /path/to/review-data`.

The workspace path appears after checkout. Open a shell there to run tests or inspect additional files. If that workspace has local edits or has moved to another revision, the app refuses to reuse it and reports the preserved path.

LEFT-side source opens a read-only snapshot from the diff merge base, including the previous filename for renamed files. RIGHT-side source opens from the PR head. Symlinks are refused as source locations.

Drafts are written atomically and scoped to the repository, PR number, head SHA, and diff-base SHA. They survive restarts. Refreshing to a different revision leaves previous drafts on disk and starts a separate set; it does not move comments to potentially unrelated lines.

GitHub access uses your existing `gh` authentication. Checkout fetches `refs/pull/<number>/head` from the base repository, so fork PRs use the same workflow. Raw Git is used only to fetch arbitrary PR refs into the isolated Jujutsu store; workspace management and revision inspection use `jj`.

## Recovering an uncertain submission

A lost connection can leave the result of a submitted review unknown. The app records each submission attempt before sending it and blocks another attempt until the previous result is resolved, including after a restart.

Use `V` to inspect published reviews and `T` to inspect inline comments, then press `u`. Choose `y` only after verifying the review was published; this clears its matching local drafts. Choose `n` only after verifying it was not published; this keeps drafts and allows a fresh preview. Recovery itself never posts anything.

## Current limits

- GitHub.com repositories only.
- Unified diffs with single-line and multiline range comments. Existing threads can be read; replying to or resolving a thread is not yet supported. PR conversation comments are not loaded.
- No textual review for binary files or files whose patch GitHub omits. Malformed or truncated hunks remain visible but cannot receive comments.
- Merge, suggestion application, and CI-log viewing are outside the review workflow.
- Working-copy edits are preserved, but the app does not offer to reset them.
- Local questions and inline replies for Codex/Claude are not implemented. Comment drafts currently belong to GitHub reviews.

## Development

```sh
cargo run -- --demo
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The test suite uses temporary local repositories and a fake `gh` executable; it does not post to GitHub. Tests require `jj`, Git, and Python 3. The diff fixtures cover old/new line coordinates, renames, absent patches, and malformed hunks. Workflow tests cover local persistence, failed writes, stale revisions, submission recovery, and request payloads.

`src/cli.rs` runs the shared command, `src/app.rs` owns navigation and dialogs, `src/ui.rs` draws the interface, `src/thread.rs` groups review replies, `src/github.rs` talks to GitHub through `gh`, `src/diff.rs` maps patch lines to comment coordinates, `src/workspace.rs` manages isolated Jujutsu checkouts, and `src/store.rs` persists local review state.
