# claustre

A TUI for orchestrating GitHub-linked AI development threads across worktrees.

Claustre gives you a terminal workbench to manage AI-assisted development workflows. It uses **git worktrees** for session isolation, **embedded PTY terminals** for live session management, **Claude Code hooks** for real-time status sync, and now includes a GitHub-first shell with **My Tasks**, **Sprint Board**, **Threads**, **Reviews**, **runtime orchestration via `sandbox.yaml`**, **workflow pipelines**, and **local-first knowledge capture**.

## Install

```bash
curl -fsSL https://claustre.pmbrull.me/install.sh | bash
```

Or build from source:

```bash
git clone https://github.com/pmbrull/claustre.git
cd claustre
cargo install --path .
```

### Prerequisites

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) -- Anthropic's CLI agent
- [gh](https://cli.github.com/) -- GitHub CLI, used by hooks to detect PRs and as the fallback GitHub auth/backend when a bundled Claustre GitHub App is not available. For Projects v2 boards, the token must include `read:project` (`brew install gh`).

## Quick Start

```bash
# Configure Claude Code permissions and check prerequisites
claustre configure

# Launch the workbench -- everything is managed from the TUI
claustre
```

The default shell is a workbench with a sidebar plus a main buffer. `Threads` and `Reviews` keep a persistent right-side inspector; browse-first views like `My Tasks` and `Sprint Board` open details in overlays instead. Use `Space t/b/h/r/a/s` to switch between `My Tasks`, `Sprint Board`, `Threads`, `Reviews`, `Agents`, and `Settings`. Use `Space g` to jump straight to `Settings -> GitHub` and `Space p` for `Settings -> AI Providers`. Use `[` and `]` to resize the focused side pane, or drag the pane borders with the mouse.

`My Tasks` now prefers the GitHub cache when Claustre has a connected GitHub user plus synced repositories. In that mode it shows the current user's assigned issues and PR-backed work items from the local GitHub cache instead of the legacy local task list. Each row renders as a compact card and surfaces synced Project v2 metadata when available, including fields such as `Type`, `Domain`, `Release`, `Priority`, and `Size`. When GitHub is not connected or no repos have been synced yet, it falls back to the older local task/task-thread view.

Launching work now goes through a thread-first modal. From `My Tasks` or `Sprint Board`, press `l` to choose the provider (`Claude` or `Codex` today), optional runtime/workflow defaults, a thread title, and extra context on top of the linked task or GitHub issue. `Threads` is now the continuation workspace: press `n` to create a new ad hoc thread for the selected repo, start typing to reply directly in the native chat surface, use `l` or `Enter` to continue/relaunch the thread, and press `o` only when you want the raw PTY terminal tab.

`Reviews` is now split into two GitHub-backed queues: `Authored PRs` and `Needs Review`. Press `t` to switch queues. In `Authored PRs`, `l` focuses or launches a continue-work thread for the selected pull request. In `Needs Review`, `l` opens the same launch modal in AI-review mode so the provider starts from a review-oriented prompt. The inspector shows cached PR bodies and a remote diff preview even before a local thread exists.

Repository switching and GitHub board switching are now separate modal pickers:
- `p` opens `Switch Repository`
- `g` opens `Switch GitHub Board` from `Sprint Board`
- pickers are searchable: start typing to filter the current list
- `Enter` confirms the current picker selection and `Esc` cancels

In `Sprint Board`, Claustre now defaults to the authenticated user's assigned
items when a GitHub login is available. Press `t` to toggle between
`Assigned to me` and `All sprint` scopes without leaving the board. Sprint
cards render multiple lines of metadata instead of a single title row so
kanban-specific fields such as `Type`, `Domain`, `Release`, `Priority`, and
assignee are visible directly in the board. Opening `Sprint Board` or changing
the selected repository/board reads from the local GitHub cache first; press
`R` only when you explicitly want to refresh the board from GitHub.

When Claustre is doing visible background work (for example preparing a session,
connecting GitHub, or loading GitHub installations), the title bar and status
line now show an animated spinner plus a short activity label so the workbench
does not look frozen.

Most settings sections are now editable from the main pane. Focus the main pane with `2`, move between sections with `j/k`, and use `Enter` or the section-specific shortcuts shown in the hint line. For example, in `Settings -> AI Providers`, `m` edits the Claude model, `e` edits effort, `r` toggles remote mode, and `u` toggles auto-update.

### First Task Walkthrough

1. **Add a project** -- press `a`, enter the project name and path to your git repository
2. **Create a task** -- press `n` to open the task form with these fields:
   - **Prompt** -- the full prompt Claude receives (what you want done)
   - **Mode** -- `supervised` (interactive), `autonomous` (hands-off), or `exploration` (open-ended)
   - **Base** -- PR target branch (defaults to project's default branch, e.g. `main`)
   - **Branch** -- git branch name (auto-generated if empty, or set to reuse an existing branch)
   - **Push** -- `pr` (create a pull request) or `push` (commit and push directly)
   - **Loop** -- review loop toggle: when on, auto-implements PR review comments
   - **Subtasks** -- optional ordered list of sub-steps for Claude to work through
3. **Launch** -- focus the tasks panel (`2`), select a pending task, press `l`, then choose the provider and any extra context in the launch modal
4. **Monitor** -- the workbench switches back to the thread-centric view and shows real-time thread/runtime status in the inspector
5. **Jump into terminals** -- press `o` on the launched thread to open the live session tab with the provider terminal and shell panes
6. **Review** -- when a PR is opened, use the inspector and browser handoff (`o`) to review it, then `r` to mark done. Merging the PR will automatically flag the task as done.

### GitHub Threads, Runtime, and Workflows

Claustre now has a backend-first GitHub/thread layer that sits alongside the existing task/session flow:

```bash
# Connect GitHub
# Uses the bundled Claustre app when available, otherwise GitHub CLI browser auth
claustre github login
claustre github status

# Hydrate GitHub issue / PR caches for a registered project
claustre github sync --project test-project

# Launch a thread from an existing task, optionally attaching runtime + workflow defaults
claustre thread launch --task-id <task-id> --provider claude --runtime-profile default --workflow plan_first_tdd

# Inspect or move the workflow / runtime forward from the CLI
claustre workflow list --project test-project
claustre workflow resume --run-id <workflow-run-id>
claustre runtime up --thread-id <thread-id>
claustre runtime health --thread-id <thread-id>
```

The current workbench exposes this state in the main pane and, where relevant, the right-hand inspector. `Threads` now renders as a rail + native chat workspace, so you can scan local threads on the left, read the latest conversation in the center, and keep diff/runtime/tests in the inspector before opening an optional live terminal tab. In `My Tasks` and `Sprint Board`, press `v` to open the selected task or GitHub item in a detail overlay instead of relying on a permanent inspector. In thread/review workspaces, move focus to the inspector (`3`) to see:

- linked thread status and provider
- workflow stage and artifact count
- runtime profile and service health
- cached GitHub issue / PR summary
- knowledge card and draft counts

For runtime automation, Claustre looks for `sandbox.yaml` in this order:

1. the configured path in `config.toml`
2. `<worktree>/sandbox.yaml`
3. `<repo>/.claustre/sandbox.yaml`
4. `~/.claustre/sandbox.yaml`

Clipboard image paste is available through `claustre thread paste-clipboard --thread-id <id>`. Images are read from the OS clipboard directly and stored under `~/.claustre/attachments/<thread-id>/...` without requiring a manual file creation step.

For ad hoc coding, open `Threads` and press `n`. Claustre will create a new local thread and worktree for the selected repo without requiring a GitHub issue. Once a thread exists, just start typing to compose inside the workbench, use `l` or `Enter` to continue it, and press `o` to open the live terminal session if one is already attached.

Inside the TUI, open `Settings -> GitHub` with `Space g`. From there:

- `Enter` or `a` connects GitHub using the best available backend
- `f` refreshes connection state and installations
- `o` opens the GitHub App install page when a bundled/custom app is available
- `y` syncs the selected repo
- `x` disconnects the current GitHub session
- `p` opens the default GitHub Project picker dropdown
- `.` shows advanced custom-app overrides
- `c/s/u/r/i` edit client ID, app slug, install URL, relay URL, and default installation only when advanced mode is shown

The `[github_app]` section is now optional. It is only needed for advanced custom-app overrides or relay experiments:

```toml
[github_app]
client_id = "Iv1.custom_app_client_id"
app_slug = "my-claustre-app"
install_url = "https://github.com/apps/my-claustre-app/installations/new"
relay_url = "https://relay.example.com"
default_installation_id = "123456"
default_project_id = "12"
```

## Key TUI Commands

**Navigation**

| Key | Action |
|-----|--------|
| `j` / `k` | Move up / down |
| `1` | Focus sidebar |
| `2` | Focus main pane |
| `3` | Focus inspector (`Threads` / `Reviews` only) |
| `Tab` / `Shift+Tab` | Cycle focus between visible panes |
| `[` / `]` | Resize the focused side pane |
| `Space t` | My Tasks |
| `Space b` | Sprint Board |
| `Space h` | Threads |
| `Space r` | Reviews |
| `Space a` | Agents |
| `Space s` | Settings |
| `Space g` | Settings -> GitHub |
| `Space p` | Settings -> AI Providers |
| `Ctrl+K` / `Ctrl+J` | Previous / next tab |
| `Ctrl+P` | Command palette |
| `c` | Configure Claude permissions |
| `?` | Help overlay |

**Task Actions**

| Key | Action |
|-----|--------|
| `n` | New task, or new ad hoc thread in `Threads` |
| `l` | Open the launch-thread modal for the selected task or board issue, focus a linked thread from task/review queues, or continue the selected thread in `Threads` |
| `i` | Optional alias for the native composer in `Threads`; typing in `Threads` starts compose automatically, and `i` still opens the skills panel elsewhere |
| `t` | Toggle `Sprint Board` scope between assigned-to-me and all sprint items, or switch the review queue in `Reviews` |
| `e` | Edit task |
| `r` | Mark done |
| `o` | Open PR / issue in browser, or open the live terminal from `Threads` |
| `d` | Delete |
| `v` | Open the task / issue detail overlay in browse views |
| `s` | Subtasks |
| `k` | Kill session |
| `a` | Add project |
| `b` | Sprint board |
| `J` / `K` | Reorder tasks |
| `1..6` | Switch inspector tabs when inspector is focused |
| `a` / `f` / `Enter` | GitHub auth / installation refresh / installation picker in `Settings -> GitHub` |

**Session Tabs**

| Key | Action |
|-----|--------|
| `Ctrl+H` / `Ctrl+L` | Focus previous / next pane |
| `Ctrl+R` | Split right |
| `Ctrl+B` | Split down |
| `Ctrl+W` | Close pane |
| `Ctrl+D` | Detach (back to workbench) |
| `Ctrl+G` | Scroll to bottom (live screen) |
| `Shift+PgUp` / `Shift+PgDn` | Scroll page up / down |

## Sync Across Machines

Claustre can sync project and task state across machines via a git repo at `~/.claustre/sync/`. Only projects, tasks, and subtasks are synced -- sessions and runtime state stay local to each machine.

### What gets synced

| Synced | Not synced (machine-specific) |
|--------|-------------------------------|
| Projects (name, default branch) | `repo_path` (different on each machine) |
| Tasks (title, description, status, tokens, PR URL, ...) | Active sessions and worktrees |
| Subtasks | Rate limit state |
| `config.toml` (copied for reference) | Sockets, PIDs, scanner data |

Projects are matched **by name** across machines. The same project can live at different paths on each laptop -- claustre handles the mapping automatically.

### Enable sync on an existing installation

If you already have claustre running with projects and tasks:

```bash
# 1. Create a private repo on GitHub (or any git host) to hold your state
#    e.g. https://github.com/you/claustre-sync

# 2. Initialize the sync repo by cloning it
claustre sync init git@github.com:you/claustre-sync.git

# 3. Push your current state
claustre sync push
```

This exports all your projects and tasks as JSON files to `~/.claustre/sync/`, commits them, and pushes to the remote. Your existing claustre setup is not modified -- sync only reads from the database.

### Enable sync on a fresh installation

If you're setting up claustre for the first time and don't have a sync repo yet:

```bash
# 1. Install and configure claustre
claustre configure

# 2. Initialize a local sync repo (no remote yet)
claustre sync init

# 3. Add a remote when you're ready
git -C ~/.claustre/sync remote add origin git@github.com:you/claustre-sync.git

# 4. Add projects, create tasks, then push
claustre sync push
```

You can also skip step 2-3 and use `claustre sync init <url>` directly if you already have the remote repo created.

### Sync a second laptop

If you already have sync set up on one machine and want to bring a second laptop up to speed:

```bash
# 1. Install claustre on the new machine
claustre configure

# 2. Clone your existing sync repo
claustre sync init git@github.com:you/claustre-sync.git

# 3. Register the same projects locally (paths will differ per machine)
claustre add-project myproject ~/code/myproject
claustre add-project another ~/work/another

# 4. Pull the synced state
claustre sync pull
```

The pull imports tasks into the matching local projects. Any synced project that isn't registered locally is skipped with a message telling you to `add-project` first.

### Day-to-day workflow

```bash
# On laptop A: finish working, push state
claustre sync push

# On laptop B: pull latest before starting
claustre sync pull

# ... work on tasks ...

# On laptop B: push when done
claustre sync push
```

`push` is idempotent -- if nothing changed, it prints "No changes to sync" and does nothing. `pull` upserts tasks by UUID, so it safely handles both new and updated tasks without duplicating anything.

### Automatic sync push

Instead of manually running `claustre sync push`, you can enable automatic syncing in `~/.claustre/config.toml`:

```toml
[sync]
auto_push = true
```

When enabled, claustre automatically pushes state to the sync repo whenever tasks are created, updated, or change status (via hooks, CLI, or TUI). The push runs as a background process, so it never blocks your workflow.

To inspect the sync directory manually:

```bash
claustre sync cd
```

This requires shell integration — add `eval "$(claustre shell-init)"` to your `.zshrc` or `.bashrc`.

## Desktop App (macOS only)

Claustre includes a native macOS desktop app built with [Tauri](https://tauri.app/). Launch it from the CLI:

```bash
claustre app
```

The desktop app is bundled in macOS release archives. If you installed via `curl | bash`, it's already at `~/.local/bin/claustre-app`. If building from source:

```bash
cargo build --release -p claustre-app
cp target/release/claustre-app ~/.cargo/bin/   # or wherever claustre is installed
```

The `claustre app` command looks for `claustre-app` next to the `claustre` binary or in `$PATH`.

## Review Loop

When a task has the **review loop** option enabled (toggle in the task form), claustre automatically monitors PR comments after the task transitions to `in_review`. A separate pane spawns in the session tab running `claustre review-loop`, which:

1. Polls the PR for new review comments at a configurable interval (default: 120s)
2. Launches Claude to evaluate each comment adversarially -- accepting bug fixes, logic errors, and security issues while rejecting nitpicks and style preferences
3. Implements accepted changes, commits, and pushes
4. Prints a summary table of accepted/rejected comments
5. Repeats until the task is marked done or rate limits are hit

### Configuration

Customize the review loop in `~/.claustre/config.toml`:

```toml
[review_loop]
# Poll interval in seconds (default: 120)
poll_interval_secs = 60

# Custom prompt (replaces the built-in prompt entirely)
# prompt = "Your custom review prompt here"
```

| Field | Default | Description |
|-------|---------|-------------|
| `poll_interval_secs` | `120` | Seconds between PR comment checks |
| `prompt` | *(built-in)* | Custom prompt for Claude when processing review comments. When omitted, uses the built-in prompt that fetches comments via `gh`, evaluates them, and implements accepted changes. |

## Sprint Board

Press `b` in the TUI (or click "Board" in the desktop app) to open a Kanban board backed by the selected repository's GitHub Project v2. Items are grouped into columns using the board column config and the project's `Status` field.

## Reviews

Press `Space r` to open `Reviews`. The main pane has two PR queues:

- `Authored PRs` lists open pull requests authored by your connected GitHub user
- `Needs Review` lists open pull requests that explicitly request your review, or PRs assigned to you that still report `review_required`

### Review Keybindings

| Key | Action |
|-----|--------|
| `t` | Toggle between `Authored PRs` and `Needs Review` |
| `j` / `k` | Navigate PR rows |
| `l` | Continue work on an authored PR, or launch an AI review thread for a review-requested PR |
| `Enter` | Open the live session when one already exists for the selected PR |
| `o` | Open the selected PR in the browser |

The inspector reuses the same `Issue`, `Diff`, `Runtime`, `Tests`, and `Plan` tabs. When no local thread exists yet, the `Diff` tab falls back to `gh pr diff` so you can inspect remote changes before launching a session.

### Board Keybindings

| Key | Action |
|-----|--------|
| `h` / `l` | Navigate columns |
| `j` / `k` | Navigate issues |
| `Enter` | Create claustre task from issue |
| `o` | Open issue in browser |
| `p` | Choose GitHub Project v2 |
| `m` | Choose sprint / iteration |
| `R` | Refresh project items |
| `Esc` / `b` | Back to dashboard |

### Board Configuration

Customize columns in `~/.claustre/config.toml`:

```toml
[[board.columns]]
name = "Backlog"
labels = []

[[board.columns]]
name = "In Progress"
labels = ["in progress", "wip"]

[[board.columns]]
name = "In Review"
labels = ["in review", "review"]

[[board.columns]]
name = "Done"
labels = []
```

Items are assigned to the first column whose name matches the Project v2 `Status` field value (case-insensitive). The first column is the catch-all for unmatched open items. The last column catches closed or merged items. Sprint filtering reads the first matching Project v2 field named `Iteration`, `Sprint`, or `Milestone`.

If the board is empty and GitHub is connected through the GitHub CLI fallback, open `Settings -> GitHub` and reconnect so `gh` refreshes the `read:project` scope. Claustre can query Projects v2 through `gh`, but only when that scope is present.

When adding a project (`a` key), you can toggle **git linked** (default: yes) to control whether the sprint board is available for that project.

### Model & Effort

Control which Claude model and reasoning effort level are used for all sessions:

```toml
[claude]
model = "claude-opus-4-6"    # default
effort = "max"               # default; valid: min, low, medium, high, max
```

| Field | Default | Description |
|-------|---------|-------------|
| `model` | `claude-opus-4-6` | Model identifier passed to `claude --model` |
| `effort` | `max` | Reasoning effort level passed to `claude --effort` |

## Documentation

Full documentation is available at **[claustre.pmbrull.me](https://claustre.pmbrull.me)**:

- [Getting Started](https://claustre.pmbrull.me/getting-started) -- installation, prerequisites, first task walkthrough
- [TUI Guide](https://claustre.pmbrull.me/tui) -- keybindings, views, session tabs, usage bars
- [Tasks](https://claustre.pmbrull.me/tasks) -- task lifecycle, modes, subtasks, autonomous chains
- [CLI Reference](https://claustre.pmbrull.me/cli) -- all subcommands for projects, tasks, skills, and stats
- [Configuration](https://claustre.pmbrull.me/configuration) -- layouts, notifications, CLAUDE.md merging
- [Architecture](https://claustre.pmbrull.me/architecture) -- hooks, SQLite store, session lifecycle
- [Desktop App](https://claustre.pmbrull.me/desktop-app) -- native macOS app

## License

MIT -- see [LICENSE](LICENSE).
