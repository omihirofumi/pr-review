# prpr

> Fully vibe-coded and intended for personal use.

A standalone Rust + Ratatui app for reviewing GitHub pull requests and local Jujutsu changes with Helix and optional Codex or Claude explanations.

Run it inside a repository to browse that repository's PRs, understand a change with an optional Codex or Claude reading guide, review diffs and inline threads, check the PR out where you are, jump into Helix, and submit a batch of inline comments from the terminal. [A day with prpr](#a-day-with-prpr) walks through a typical day.

## Install

### Homebrew

```sh
brew tap omihirofumi/tap
brew install omihirofumi/tap/prpr
```

The formula builds the tagged release from source using Rust as a build dependency. It does not install runtime tools; prpr uses the executables already on your `PATH`. Update it with `brew update` followed by `brew upgrade omihirofumi/tap/prpr`.

Install only the tools needed for the features you use, using your preferred installation method:

| Feature | Tools on `PATH` |
| --- | --- |
| GitHub PR browsing and reviews | [GitHub CLI](https://cli.github.com/) (`gh`), authenticated with `gh auth login`; Git (`git`) for repository discovery |
| Local Jujutsu review | [Jujutsu](https://docs.jj-vcs.dev/latest/install-and-setup/) (`jj`) |
| Isolated PR checkout | Jujutsu (`jj`) and Git (`git`) |
| Open source in Helix | [Helix](https://docs.helix-editor.com/install.html) (`hx`) |
| AI explanations | Codex or Claude CLI, installed and authenticated separately |

Opening GitHub source in the isolated review workspace (`[editor] open_in = "workspace"`) also requires the isolated PR checkout tools. When using a custom [`HELIX_RUNTIME`](https://docs.helix-editor.com/building-from-source.html), keep it compatible with the `hx` binary on your `PATH`.

### Cargo

Install Rust to build prpr, then install the runtime tools for your chosen features from the table above.

From the source checkout:

```sh
cargo install --path . --locked
```

## Local Jujutsu review

```sh
cd /path/to/your/repository
prpr diff                       # Current working-copy change (@)
prpr diff -r @-                 # Another revision or connected revset
prpr diff --from main --to @    # Compare two revisions
prpr diff --agent codex          # Override the AI provider for this launch
```

Local reviews open directly in Diff. Use the same `j/k`, `gg/ge`, `[/]`, `v/x`, `+/-`, `F`, and `gf/e` controls as PR reviews. `Q` asks about selected lines or the current hunk; `E` explains it. `1` opens Overview with the change description; `2` opens AI Guide, where `a` prepares a structured reading guide. AI configuration is shared with PR reviews.

Press `c` to write a local note on a line or range, then `Ctrl-s` to save. `N` lists notes: `c` replies, `o` jumps to the source, and `d` deletes the selected note and its replies. `t` toggles inline notes; `T` reads full threads. Local notes persist across restarts. After `r` refreshes a native `prpr diff` review, notes on changed files are marked outdated rather than attached to different code.

`gf` opens the actual working file when it still matches the reviewed right-hand source. Removed lines, historical versions, and files edited since loading open a read-only snapshot instead. Binary or oversized files remain listed without text/AI context; text is limited to 2 MiB per side and 100000 lines. Local review does not require GitHub authentication and has no GitHub submission action.

### Launch from `jj diff`

Register prpr as a Jujutsu external viewer:

```sh
jj config set --user merge-tools.prpr.program prpr
jj config set --user merge-tools.prpr.diff-args '["difftool", "$left", "$right"]'
jj --no-pager diff --tool prpr
jj --no-pager diff -r @- --tool prpr
```

To make plain `jj diff`, `jj show`, and `jj interdiff` open prpr while other commands such as `jj log -p` keep text diffs, add a scoped section to your Jujutsu configuration (`jj config path --user`):

```toml
[merge-tools.prpr]
program = "prpr"
diff-args = ["difftool", "$left", "$right"]

[[--scope]]
--when.commands = ["diff", "show", "interdiff"]
[--scope.ui]
diff-formatter = "prpr"
paginate = "never"   # return to the shell, not a pager, after closing the TUI
```

Without a terminal, as when an agent or script runs `jj diff`, `prpr difftool` prints a Git-style diff of the two directories instead of opening the TUI, so tools that read `jj diff` output keep working. Explicit formats such as `jj diff --git` or `--stat` are unaffected.

For one-off use without changing the default, `--no-pager` avoids returning to Jujutsu's own pager after closing the TUI.

The external viewer reads the exact temporary directories supplied by Jujutsu. Reopen `jj diff` to refresh that snapshot; use native `prpr diff` for refreshes inside the TUI. Repository association uses the launching shell's `PWD`. When a wrapper uses `jj -R` to review a different repository, add `--repo-root /absolute/repository/path` to that wrapper's `difftool` arguments.

## Review with an agent

Install the bundled skill for Codex and Claude:

```sh
prpr skill install
```

`prpr skill install --force` replaces an older copy when updating the skill. It installs `prpr-review/SKILL.md` under `~/.agents/skills` and `~/.claude/skills`. Start a new agent session if it has already cached its skill list. With the TUI open, ask the agent **“Use prpr-review to read my comments and reply.”**

Local `jj` reviews use `c` for notes. In a GitHub PR, use `C` for an agent-facing local note; lowercase `c` retains its GitHub draft behavior. Agent replies appear inline and in `N`, and stay local.

Agents communicate with the running viewer through JSON CLI commands:

```sh
prpr session list --json
prpr session review --repo .
prpr session review --repo . --file src/main.rs --include-patch
prpr session context --repo .
prpr session comment list --repo .
prpr session comment add --repo . --reply-to note-ID --body 'Explanation' --author codex
prpr session navigate --repo . --file src/main.rs --new-line 42
prpr session mark add --repo . --file src/main.rs --new-line 42 --start-column 9 --end-column 21
prpr session mark clear --repo .
prpr session reload --repo .
```

Use `--session ID` when several viewers share a repository. If the viewer uses `--workspace-root`, pass the same option to session commands. Sessions use private local Unix sockets; no daemon or network listener is needed. `comment list` includes local notes, GitHub comments, and saved GitHub drafts separately. Replies target local note IDs. `mark add` highlights exact characters (1-based, inclusive columns) or whole lines until the review reloads, so an agent can point at the expression a note is about. `reload` refreshes the review as `r` does and keeps you on the Diff tab. Commands reject unknown files, invisible line ranges, and outdated note anchors. Reading comments or replying to them does not authorize an agent to edit code or publish a GitHub review.

## Quick start

Run the app from the repository you want to review, not necessarily this project's checkout:

```sh
cd /path/to/your/repository
prpr
```

Select a PR with `j` / `k` and press `o`. If you already know its number, open it directly with `prpr 123`. Subdirectories work too; the app detects the containing repository.

The PR list has a compact table and a preview pane showing the selected PR's Markdown description. `j` / `k` changes the selected PR; `h` / `l` or `Tab` focuses the table / preview. Press `p` to hide or show the preview, `P` to switch its position, and `[` / `]` or `1` / `2` to switch between **Description** and **AI guide**. Narrow terminals place the preview below the table. `Ctrl-d` / `Ctrl-u` scroll the preview. Descriptions arrive with the list; browsing them does not load diffs or start an AI agent. Press `a` to prepare or reuse a guide for the selected PR.

Press `o` or `Enter` in the list to open **Diff**. A direct target such as `prpr 123` opens **Overview**, showing the title, author description, and change counts as soon as metadata arrives. Labels, CI status, conversation, diff, and inline comments load independently in the background. Inside an opened PR, press `1` for Overview, `2` for AI Guide, `3` for Diff, or `Tab` / `Shift-Tab` to cycle. With an AI agent configured, a structured reading guide appears in AI Guide without blocking navigation; the tab label shows whether it is loading, ready, or failed.

In Diff:

1. Press `j` / `k` to move through the current file; the cursor stops at its first and last lines. Press `.` / `,` (or `n` / `p`) for the next / previous file, and `gg` / `ge` for the current file's top / bottom. The view scrolls only when the cursor comes within `scrolloff` rows (default 5) of an edge; `Ctrl-d` / `Ctrl-u` move the view and cursor together.
2. Press `gf` or `e` to open the selected source line in Helix. Quit Helix to resume the review at the same position.
3. Press `c` to comment on one line. For three lines, press `xxx`, then `c`; for a variable range, press `v`, move with `j` / `k`, then press `c`. In the diff view, `Esc` or `;` clears a selection. Ranges cannot combine removed and added lines or span separate hunks.
4. Write the comment and press `Ctrl-s` to save a **local draft**. `Ctrl-h` deletes backward. Press `D` to inspect or edit saved drafts.
5. To publish the review, press `s`, write its summary, and use `Tab` to choose comment, approve, or request changes. Press `Ctrl-s` to preview, then `Enter` to send it to GitHub. `Esc` cancels the preview.

`Ctrl-s` inside the comment editor does not publish anything. The final preview's `Enter` is the publishing action.

Press `t` to hide/show inline comments, `T` to read complete threads, and `V` to read published review summaries. Press `#` to open another PR by number, `?` for the keyboard guide, or `q` to quit. After installing an update, restart the app to use the new version.

To learn the controls without contacting GitHub, run `prpr --demo`. It starts on the PR list and offers a sample AI guide and sample answers, local drafts, and review previews. Source checkout and Helix opening require a real PR.

## A day with prpr

One reviewer's Wednesday, to show how the pieces fit together. The repository, PRs, and files are made up.

### 9:10 Today's PRs are chosen before a browser opens

In the repository, run `prpr --search 'review-requested:@me'`. Only PRs waiting for your review are listed, with the selected PR's description beside them. Walk the list with `j` and press `2` to preview a PR's AI guide; guides prepared earlier come from the cache, so there is no wait.

Three today. Start with the heaviest, #482. `o` would open Diff, so type `#` and the number to start from Overview instead.

### 9:20 CI and conversation without leaving the terminal

Overview shows two labels, `backend` and `needs-tests`, and `CI 1 failed · 13 passed`. Re-runs of the same check count once, by their latest run, and the failing `integration / test` check is listed above the description.

The description reads in full. Template hints such as `<!-- Describe the change -->` stay hidden, as on GitHub, so only what the author wrote remains. At the end, the conversation lists a bot's issue-link comment and an earlier review, in time order.

### 9:30 The AI Guide sets the reading order

Press `2` for AI Guide: summary, intent, risks, and a reading order in the right column. Step 1 is `services/api/src/retry/policy.rs:60`, the core rule of the change. `Enter` puts the cursor on that line in Diff; when done, `2` returns and `n` moves to the next step.

In Diff, `j` moves the cursor while the view stays still, and scrolling starts only five rows before the bottom edge. The cursor stops at the end of a file, and `.` moves to the next one. The sidebar joins single-child folders into rows such as `services/api/src`, so it is always clear whether you are under `main/…` or `test/…`.

On an interesting line, `gf` opens that file from your checkout in Helix. A deleted line opens a read-only copy of the diff base fetched from GitHub. Nothing has to be prepared first.

### 10:05 Each question goes to whoever can answer it

The condition in `should_retry` is puzzling. To check your own understanding, press `x` three times to select the three lines and `E` for an explanation. It appears in a side panel; nothing reaches GitHub.

This PR was written by your coding agent, which still has the context of why it wrote the code. So press `C` on the same lines to leave a local note, and ask the agent: "Use prpr-review to read my comments and reply." The reply appears under the note a moment later, and `session mark` highlights the exact expression it refers to. Local notes stay on your machine.

A question for the human reviewers goes into a GitHub draft with `c` instead. `Ctrl-s` only saves it locally; nothing is sent yet.

To ask another team on Slack, `y` copies the selection as a Markdown snippet headed by `file:line`, ready to paste. `gy` copies a GitHub permalink pinned to the PR head instead.

### 10:20 An AI review runs in the background

With a first pass done, press `R` to run Claude's `/code-review` on the PR. It takes a few minutes, and Diff stays usable meanwhile.

The status line warns that your checkout is not at the PR head: you were still on another branch, so line numbers could point at other code, and findings would stay text-only. Press `w` to check PR #482 out right there; when the first result arrives, open it with `R` and press `r` to rerun. This time each finding with a file and line is placed as a `claude review` note on that line: a retry that can send twice, and a race between the scheduled job and the API. Neither was in your drafts.

### 13:30 Your own change, explained by the agent that wrote it

In the afternoon you return to your own work: a retry change your coding agent wrote this morning, to read before pushing. `prpr diff` opens it with the same controls as a PR.

Why is the backoff capped at 30 seconds? Write a note with `c` and ask the agent the same way. The reply says the cap matches the upstream API timeout, and a mark highlights the constant it comes from.

You would rather make the cap configurable. The agent edits the code and runs `prpr session reload`; without a key press, the review refreshes and stays on Diff.

### 17:40 One review, sent in the evening

Back on #482, `D` lists the morning's drafts. Add the one AI finding you agree with, in your own words. `s` opens the summary, `Tab` selects Request changes, `Ctrl-s` shows the preview, and `Enter` sends it.

`V` now shows your review at the end of the conversation. Looking back, the browser never opened all day.

## Command palette and file sidebar

Press `Ctrl-k` to open the command palette from the PR list, Overview, or Diff. Type words such as `sidebar`, `next hunk`, `explain diff`, or `checkout` to filter actions. Use arrows (or `Ctrl-n` / `Ctrl-p`) to select, `Enter` to run, and `Esc` or `Ctrl-k` to close. `Ctrl-u` clears the search; `Ctrl-h` deletes backward. Available actions follow the current view. Disabled actions show why they cannot run. Review submission still uses its existing summary and final confirmation screens.

The file sidebar shows changed files as a tree. Chains of directories that each contain a single subdirectory are joined into one row, such as `services/api/src`, so deep package paths stay readable; long joined names keep their first and last directories, such as `main/…/example`.

`Ctrl-b` hides or shows the file sidebar. Hiding it gives the diff the full width and preserves the current file, cursor, and selection. The setting lasts for the current app session, including file changes and refreshes. Press `h` to show and focus the file tree again; `l`, `Enter`, `Tab`, or `→` returns to the diff. Returning to a different file than the one you left starts at its top, while returning to the same file keeps your position. These shortcuts leave comment/question editors unchanged while you are typing.

Use `]` for the next hunk and `[` for the previous hunk, or run those actions from the palette. At a file boundary, navigation continues to the next/previous file with a textual hunk, skipping unavailable patches. An actively extending selection stays within its current file. Original hunks remain separate navigation targets when expanded context merges them or the full file is shown.

## Unified and split view

Diff starts in the unified layout. Press `|` to switch to a side-by-side split for the rest of the session: deletions on the left with old line numbers, additions on the right with new line numbers, and unchanged lines on both sides. A run of deleted lines pairs row by row with the added lines that follow it, and the shorter side is padded.

In split view, `j` / `k` move row by row as in Hunk: within a row the cursor visits the left (deleted) line before the right (added) one, and `k` reverses that order, so it never jumps back up inside a replaced block. Unchanged lines take one step per row. Positions still refer to the same diff lines, so selections, comments, notes, marks, and `gf` work the same in both layouts. Choose the starting layout in the configuration:

```toml
[diff]
layout = "auto"   # "unified" (default), "split", or "auto": split from 160 columns
```

Split view needs a diff pane at least 60 columns wide and otherwise falls back to unified.

## Configurable shortcuts

Normal-view shortcuts are configured in the same file as AI settings: `~/.config/prpr/config.toml` (or `$XDG_CONFIG_HOME/prpr/config.toml`). Run `prpr --config-path` to locate it. Add a `[keybindings]` table, then restart prpr:

```toml
[keybindings]
command_palette = ["Ctrl-k"]
toggle_sidebar = ["Ctrl-b"]
previous_hunk = ["["]
next_hunk = ["]", "Alt-j"]
open_file = ["gf"]
```

Each array replaces **all** shortcuts for that action. Omitted actions keep their defaults. An empty array disables the action's shortcuts while keeping its command-palette action available:

```toml
[keybindings]
command_palette = ["Ctrl-p", "F2"]
toggle_sidebar = ["Space b"]
open_file = []
```

Bindings apply to the PR list, Overview, AI Guide, and Diff. Dialog navigation and comment/question editing controls stay fixed, including `Esc`, `Ctrl-s`, and `Ctrl-h`. `o` does **not** open source files in Diff by default; `gf` and `e` do. It still opens PRs in the list and reading-guide steps in AI Guide.

Use characters directly (`Q`, `]`, `+`), named keys (`Enter`, `Tab`, `Shift-Tab`, `Space`, arrows, `Home`, `End`, `PageUp`, `PageDown`, `F1`–`F24`), and `Ctrl-`, `Alt-`, or `Shift-` combinations. Use the resulting character directly for shifted punctuation. Multi-key sequences use spaces, such as `Space b` or `g p`; familiar lowercase sequences such as `gf` also work. Sequences contain up to four keys, and `Esc` cancels a pending sequence.

`Ctrl-c` remains reserved for immediate exit. Terminal aliases (`Ctrl-i`/`Tab`, `Ctrl-j` or `Ctrl-m`/`Enter`, `Ctrl-h`/`Backspace`, and `Ctrl-[`/`Esc`) are checked together. Duplicate keys and ambiguous prefixes are rejected when their actions share a view. For example, assigning `next_hunk = ["n"]` requires rebinding or disabling `next_file`, while AI Guide's `next_reading_step` may still use `n`.

The palette, help, and footer display the effective keys. Inspect every action name and its applicable views without opening the TUI:

```sh
prpr --print-keybindings
prpr --config /dev/null --print-keybindings  # Defaults, independent of your config
```

Changing the AI provider from the app preserves `[keybindings]`.

## Command options

```sh
prpr                                      # Open PRs in the current repository
prpr 123                                  # PR #123 in the current repository
prpr --search 'author:@me'                 # Filter PRs in the current repository
prpr --repo owner/repo                     # Explicit repository override
prpr --repo owner/repo 123
prpr https://github.com/owner/repo/pull/123
prpr --demo                               # Offline example; never posts
prpr 123 --agent codex                     # Codex guide for this launch
prpr 123 --agent claude --model MODEL      # Claude with a model override
prpr 123 --agent none                      # Read without an AI guide
prpr --config-path                        # Print the configuration path
prpr --print-keybindings                  # Print effective shortcuts and action names
prpr --config /path/to/config.toml 123     # Use another configuration file
```

A repository is detected from the current directory, including subdirectories. A PR URL supplies its own repository. The inbox, refresh, search, and direct number entry stay scoped to that selected repository; there is no account-wide review queue. Outside a repository, use `--repo` or a PR URL.

The inbox shows up to 100 open PRs. `/` filters the loaded inbox locally; `--search` queries GitHub within the selected repository. Press `#` from the inbox or review screen to open a PR by number without restarting.

Description previews, full descriptions, Ask responses, and AI Guide use a shared dark Markdown theme. Headings, emphasis, links, tables, and inline code are styled; fenced code keeps its indentation in a shaded block. Prose wraps to a comfortable reading width, and scrollbars show the position in long content. AI guide sections use distinct headings and spacing.

## Overview and AI reading guide

Press `A` in the PR list or any PR tab to choose **Codex**, **Claude**, or **None**. The choice is saved to `~/.config/prpr/config.toml` (or `$XDG_CONFIG_HOME/prpr/config.toml`). Use `prpr --config-path` to locate your configuration. A fresh installation defaults to None; selecting an agent enables automatic explanations. The provider uses its existing CLI login, so install and sign in to `codex` or `claude` separately. Homebrew does not install an AI CLI.

Example configuration:

```toml
[ai]
provider = "codex"       # "codex", "claude", or "none"
language = "en"          # e.g. "ja" for Japanese explanations
auto_explain = true
timeout_seconds = 180
max_input_bytes = 160000
review_timeout_seconds = 1200  # AI review (R) runs the provider's own review and takes longer
# model = "MODEL"        # Omit to use the provider CLI's default model
```

`--agent` and `--model` override settings for one launch. Choosing another provider with `A` clears the saved model override so a provider-specific model is not reused accidentally. Set `auto_explain = false` to generate only when you press `a`.

The explanation contains:

- **Summary:** what the PR changes.
- **Intent:** why, labeled as author-stated, inferred from the diff, or unknown.
- **Changes:** the main behavior and affected files.
- **Reading order:** an ordered route through the implementation with file/line references.
- **Risks:** concrete areas to inspect during review.
- **Verification:** visible test evidence, suggested checks, and unknowns. No tests are run by the explanation agent.
- **Coverage:** which complete patches were included, omitted by the context budget, or unavailable from GitHub.

Overview shows what GitHub and people say about the PR: author and state, labels, a CI summary (latest run per check), the full author description, and the conversation. Failed and pending checks are listed above the description. The conversation merges PR comments and reviews with text in time order; minimized comments and reviews that only carry inline comments are omitted. HTML comments, such as PR template hints, are hidden as on GitHub. `r` refreshes everything, including CI.

AI Guide shows what the agent says. There, `j` / `k` scroll, `gg` / `ge` jump to the beginning/end, and `n` / `p` select a reading-guide step. Press `Enter`, `o`, or `gf` to jump to that step in Diff; press `gf` or `e` again there to open it in Helix. Labels, CI results, and the conversation are not sent to the agent.

Guides are generated from PR metadata and the supplied diff in an isolated temporary directory. Codex runs with a read-only sandbox; Claude runs without tools or MCP servers. PR contents are treated as untrusted input. The guide does not check out code, run tests, or publish reviews. Selecting an agent sends the supplied PR content to that provider through its CLI and uses your existing account's usage allowance.

Explanations are cached under the application data directory. The cache is tied to the PR metadata, head and diff base, supplied context, provider, model, language, and context budget. Reopening the same unchanged PR reuses its guide. In an opened PR, `a` explicitly regenerates it; in the PR list, `a` prepares or reuses the selected PR's guide. Leaving the PR, changing providers, or quitting cancels an active generation. A missing CLI, login error, timeout, or invalid AI response is shown in AI Guide while Overview and Diff remain usable. File and line references are validated against the supplied patch; the explanation still needs human review.

## AI code review

Press `R` in an opened GitHub PR to run your provider's own review command, so prpr follows each provider's review practice instead of a prompt of its own:

- **Claude** runs `/code-review <PR number>` in print mode with read-only tools only (`Read`, `Grep`, `Glob`, subagents, and `gh pr view/diff/checks` and `git diff/log/show/blame`-style commands). Anything else, including posting to GitHub or editing files, is denied.
- **Codex** runs `codex exec review --base <diff base>` in a read-only sandbox. Codex reviews the working tree, so it runs only when your checkout's HEAD is the PR head; press `w` in Diff to get there.

The review runs in the background from the repository you launched prpr in; keep reviewing meanwhile. When it finishes, press `R` to read it and `r` there to rerun. When the review lists findings with file and line, and your checkout is at the PR head, each finding is also placed as a local note on its line (author `claude review` or `codex review`; `t` shows, `N` lists). If your checkout is elsewhere, line numbers may refer to other code, so findings stay text-only. Results are kept per PR head for the session, survive refreshes, and never post to GitHub. Local diffs (`prpr diff`) do not support AI review.

## Ask your agent and explain a diff

**Ask** and **Explain diff** use the configured Codex or Claude CLI and its existing login. They share the guide's language, model, timeout, and context budget.

The conversation lives in an AI chat panel on the right, next to whichever tab is open, so the code stays visible while you ask about it. Press `Ctrl-l` on Overview, AI Guide, or Diff to show or hide it. Terminals narrower than 120 columns show the panel over the review instead.

- Press `Q` to type in the panel. On Diff, the line under the cursor, or the lines selected with `v` or repeated `x`, are attached to the question when you send it; the panel shows what is attached. On Overview and AI Guide the whole changeset is attached. AI questions can include both removed and added lines.
- Press `E` to immediately explain the attached lines: behavior before and after, purpose, how the change works, and review concerns.
- Press `Ctrl-s` to send. `Enter` inserts a newline; `Ctrl-h` deletes backward.
- `Esc` returns to the review and leaves the panel open, and a running request keeps going. Select other lines and press `Q` again to ask a follow-up about them in the same thread.
- `Ctrl-x` cancels a running request, and `Ctrl-n` starts a new thread. `Ctrl-d` / `Ctrl-u` and PageDown / PageUp scroll the conversation.

Each turn shows its source scope, file/line references, unknowns, and context coverage. Questions include the selected lines, surrounding complete hunks, PR description, and additional patches that fit the budget. References are checked against the supplied diff. Older conversation turns are omitted when necessary to fit the budget, and that omission is reported.

Conversations remain in memory for the current open PR revision; leaving the PR or changing agents clears them. Use `c` for persistent GitHub-review drafts and `Q` for local agent questions. AI explanations and questions do not publish reviews or run tests. A failed request restores the question for editing and retry.

## Review controls

Comments on deleted lines target the LEFT side; added and context-only ranges target the RIGHT side. Multiline comments must stay within one complete hunk on a single side; a selection containing both deleted and added lines cannot be submitted as one range. Both the displayed revision and comment coordinates are checked before submission. A changed PR head or diff base requires a refresh with `r`.

`T` opens the full inline thread list, including outdated comments and comments outside the displayed patch. `V` shows published review summaries. `?` shows the keyboard guide. Long operation messages can be read with `!`.

Navigation and selection follow Helix's line-oriented workflow: `gg` goes to the top of the current file, `ge` to its end, and `gf` opens the selected source location in Helix. `v` toggles selection-extension mode; while active, movement extends the range and stays in the current file. Toggling `v` off retains the range until the next normal movement. `x` selects the current line, and repeated `x` presses extend downward. `X` selects whole-line bounds without adding another line. `;` or `Esc` clears the range. Starting a selection on a hunk header moves to a nearby numbered source line.

| Key | Action |
| --- | --- |
| `1` / `2` / `3`, `Tab` / `Shift-Tab` | Overview / AI Guide / Diff tabs |
| `h` / `l` | Focus files / diff pane in Diff |
| `a` / `A` | Prepare AI guide / choose and save an agent |
| `Ctrl-k` | Search and run commands for the current view |
| `Ctrl-b` | Toggle the file sidebar |
| `Ctrl-l` | Toggle the AI chat panel |
| `Q` / `E` | Ask the agent / explain selected diff lines or the current hunk |
| `j` / `k`, up / down | Move through lines within the current file |
| `.` / `,`, `n` / `p` | Next / previous file |
| `]` / `[` | Next / previous hunk, continuing across files |
| `gg` / `ge` | Top / end of current file, or first / last item in lists |
| `gf` | Open selected source location in Helix |
| `G`, Home / End | Legacy file or list boundary shortcuts |
| `v` | Toggle selection-extension mode |
| `x` / `X` | Select/extend whole lines downward / select whole-line bounds |
| `;` | Collapse the selection at the cursor |
| `Ctrl-d` / `Ctrl-u`, PageDown / PageUp | Move through the diff |
| left / right | Scroll long diff lines horizontally |
| `b` / `B` | PR description / open PR on GitHub |
| `y` / `gy`, `Y` | Copy selected lines as Markdown / copy a GitHub permalink pinned to the reviewed revision |
| `R` | Run or open the provider's AI code review |
| `\|` | Toggle unified / split view |
| `+` / `-` | Expand 20 context lines / collapse current context |
| `F` | Toggle full-file context |
| `t` | Toggle inline comments, replies, and local drafts |
| `T` / `V` | Full inline thread list / PR conversation and reviews |
| `#` | Open a PR by number in the selected repository |
| `o` | Open the selected PR or saved item |
| `w` / `e` | Check out the PR in this repository / open source in Helix |
| `c` / `D` | Compose comment / manage saved drafts |
| `s` | Preview and submit review |
| `u` | Recover a submission with an unknown result |
| `!` / `?` | Operation details / keyboard guide |
| `r` | Refresh inbox or current PR |
| `Esc` / `q` | Clear a range, otherwise return to inbox / quit |

The Hunk Review-inspired layout uses a compact file sidebar, separate hunk headers, and shaded addition/deletion rows. The selected source line is highlighted across its full width, with distinct selection colors for added and deleted lines. Comment counts remain visible when the inline threads are hidden. Toggling threads preserves the selected source line, including its comment and Helix coordinates. Long inline cards show a preview; use `T` to read the full published thread, or `D` and `o` to open the full draft.

The terminal needs at least 48 columns and 12 rows; 100 columns or more gives the diff more space. Paste is supported in comment, summary, and PR-number editors. Arrow keys, Home, End, Backspace, `Ctrl-h` (backspace), and Delete edit text.

## Expand unchanged context and open GitHub

Diff starts with GitHub's original patch. Press `+` to reveal 20 more unchanged lines before and after the current hunk or visible hunk group. Nearby hunks merge when their expanded context overlaps. Press `-` to collapse that group's extra context. `F` toggles the full file; toggling it off restores the previous expansion. Pressing `-` in full-file mode returns to the collapsed diff.

Hidden regions show the number of unchanged lines. The selected source location and selection endpoints stay tied to their original line numbers as context expands. `j` / `k`, `gg` / `ge`, hunk navigation, comments, and Helix opening continue to use those coordinates. If collapse hides the cursor's line, the cursor moves to the nearest visible source line.

Source loads only when requested, using the exact PR head and diff merge-base revisions. Renames use the previous filename on the base side. Validated source is cached by repository, revisions, and file identity, so repeated expansion does not fetch it again. This uses GitHub's file APIs and does not prepare a checkout or fetch Git history. Loading context leaves the rest of the interface usable.

Full-file context supports UTF-8 text without NUL bytes, up to 2 MiB and 100,000 lines per side. The patch must match both source revisions and describe the changes between them. Unsupported, incomplete, or unavailable content leaves the original diff usable; `gf` can still open a review workspace in Helix.

Expanded lines can be selected, opened in Helix, or used with `Q` / `E`. For selections within one displayed hunk, agent questions use the selected region and up to 20 nearby source rows. Selections spanning multiple hunks include those hunks and remain subject to the configured context budget. GitHub review drafts remain limited to the original patch's complete hunks.

Press `B` in the PR list, Overview, or Diff to open the selected PR in your configured browser through GitHub CLI. The shortcut follows the selected repository and PR number. In text editors and the list filter, `B` remains ordinary text input.

## Checking out a PR

Press `w` in Diff to check the PR head out in the repository you launched prpr from, so tests, editors, and AI review see the reviewed code. prpr fetches `refs/pull/<number>/head` from the base repository, so fork PRs work too, and stores it as the remote-tracking ref `prpr/pr-<number>` instead of creating a local branch.

- **Jujutsu:** creates a new change on top of the PR head (`jj new`). The PR commit shows as the immutable bookmark `pr-<number>@prpr`, and your previous change stays where it was.
- **Git:** switches to the PR head with a detached HEAD. If tracked files have uncommitted changes, prpr refuses and asks you to commit or stash them first.

If prpr was started outside a checkout of the PR's repository, `w` prepares an isolated review workspace instead (see below).

## Local workspaces and drafts

The application creates its own Jujutsu repositories and revision-specific review workspaces in the platform's local application-data directory. These workspaces back `[editor] open_in = "workspace"` and `w` outside a checkout of the PR's repository; they never change the repository you launched prpr from. Override the data location with `--workspace-root /path/to/review-data`.

The workspace path appears after checkout. Open a shell there to run tests or inspect additional files. If that workspace has local edits or has moved to another revision, the app refuses to reuse it and reports the preserved path.

Drafts are written atomically and scoped to the repository, PR number, head SHA, and diff-base SHA. They survive restarts. Refreshing to a different revision leaves previous drafts on disk and starts a separate set; it does not move comments to potentially unrelated lines.

GitHub access uses your existing `gh` authentication. Checkout fetches `refs/pull/<number>/head` from the base repository, so fork PRs use the same workflow. Raw Git is used only to fetch arbitrary PR refs into the isolated Jujutsu store; workspace management and revision inspection use `jj`.

## Opening GitHub PR source in Helix

By default, `gf` / `e` open the repository you launched prpr from, as it is now:

- A RIGHT-side (added or context) line opens that file in your checkout at the PR's line number. Your checkout may be on another revision, so the content can differ from the PR head; the status line says so after Helix exits.
- A LEFT-side (deleted) line opens a read-only copy of the diff merge base fetched from GitHub, using the previous filename for renamed files.
- A file missing from your checkout, or a PR from a different repository than the launch directory, opens a read-only copy of the PR head.

To open source in an isolated review workspace pinned to the PR head instead, opt in:

```toml
[editor]
open_in = "workspace"   # default: "repository"
scrolloff = 5           # rows kept between the Diff cursor and the viewport edge
```

In workspace mode, the first `gf` prepares the workspace (as `w` does). LEFT-side source opens a read-only snapshot from the diff merge base, including the previous filename for renamed files; RIGHT-side source opens from the PR head. Symlinks are refused as source locations.

## Recovering an uncertain submission

A lost connection can leave the result of a submitted review unknown. The app records each submission attempt before sending it and blocks another attempt until the previous result is resolved, including after a restart.

Use `V` to inspect published reviews and `T` to inspect inline comments, then press `u`. Choose `y` only after verifying the review was published; this clears its matching local drafts. Choose `n` only after verifying it was not published; this keeps drafts and allows a fresh preview. Recovery itself never posts anything.

## Current limits

- GitHub.com repositories only.
- Unified diffs with single-line and multiline range comments. Existing threads can be read; replying to or resolving a thread is not yet supported. PR conversation comments are read-only.
- No textual review for binary files or files whose patch GitHub omits. Malformed or truncated hunks remain visible but cannot receive comments.
- Merge, suggestion application, and CI-log viewing are outside the review workflow.
- Working-copy edits are preserved, but the app does not offer to reset them.
- AI conversations are local to the current open PR session; exporting or persisting them across restarts is not yet supported.

## Development

```sh
cargo run -- --demo
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The test suite uses temporary local repositories and fake `gh`, `codex`, and `claude` executables; it does not post to GitHub or call a live AI provider. Tests require `jj`, Git, and Python 3. The diff fixtures cover old/new line coordinates, renames, absent patches, and malformed hunks. Workflow tests cover metadata-first loading, stale background results, agent cancellation and caching, structured output validation, local persistence, failed writes, stale revisions, submission recovery, and request payloads.

`src/cli.rs` runs the shared command and background jobs, `src/app.rs` owns navigation and dialogs, `src/ui.rs` draws the interface, `src/settings.rs` persists configuration, `src/analysis.rs` runs and validates AI explanations, `src/assistant.rs` supplies focused questions and follow-ups, `src/thread.rs` groups review replies, `src/github.rs` talks to GitHub through `gh`, `src/diff.rs` maps patch lines to comment coordinates, `src/workspace.rs` manages isolated Jujutsu checkouts, and `src/store.rs` persists local review state.
