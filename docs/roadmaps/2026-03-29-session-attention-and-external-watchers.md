# Claustre Session Attention and External Watchers

## Summary

Claustre should absorb the best ideas from `opensessions` without copying its
tmux-first architecture. The goal is not to turn Claustre into a sidebar for a
multiplexer. The goal is to make Claustre's existing session, thread, review,
runtime, and worktree model feel as alive and navigable as the best
session-orchestration tools.

The capability we want is simple:

- always know which session needs you
- always know what repo/branch/runtime that session belongs to
- switch to the right thread or terminal instantly
- keep GitHub-first review and worktree flows intact
- treat managed and external sessions as first-class signals in the same UI

Claustre already has most of the right primitives:

- managed session/worktree lifecycle in [`src/session/mod.rs`](/Users/harsha/Code/claustre/src/session/mod.rs)
- detached PTY ownership in [`src/session_host.rs`](/Users/harsha/Code/claustre/src/session_host.rs)
- thread launch/orchestration in [`src/threads.rs`](/Users/harsha/Code/claustre/src/threads.rs)
- external session scanning in [`src/scanner/mod.rs`](/Users/harsha/Code/claustre/src/scanner/mod.rs)
- external-session persistence in [`src/store/queries/external_sessions.rs`](/Users/harsha/Code/claustre/src/store/queries/external_sessions.rs)
- paused/waiting detection in the Rust TUI under [`src/tui/app/mod.rs`](/Users/harsha/Code/claustre/src/tui/app/mod.rs)
- thread/session UI surfaces in [`src/tui/ui/workbench.rs`](/Users/harsha/Code/claustre/src/tui/ui/workbench.rs) and [`src/tui/ui/session.rs`](/Users/harsha/Code/claustre/src/tui/ui/session.rs)

What is missing is a coherent attention model and the UI/DB wiring to present
that model consistently.

## Goals

- Surface session attention states directly inside the native Rust session and
  thread workspace.
- Preserve Claustre's current differentiators:
  - PR launch aligned to a worktree
  - review queues and authored/needs-review flows
  - provider-aware session restore/relaunch
  - runtime integration
  - Neovim/manual editing
- Extend external session support beyond passive discovery and make it usable as
  a first-class navigation signal.
- Add localhost/port visibility where runtime-backed work happens.
- Make "what needs me now?" a fast answer in the title bar, sidebar, thread
  list, and session tabs.

## Non-Goals

- Replacing Claustre's Rust UI with a tmux plugin or Bun/OpenTUI app
- Making external sessions equal to managed sessions in control surface
- Replacing the current thread/review/worktree model
- Introducing a native desktop shell in this milestone

## What We Should Borrow From opensessions

From the verified `opensessions` README:

- live agent state across providers
- unseen markers for important state changes
- branch/cwd/session context in the switcher
- detected localhost ports
- quick switching to the terminal that matters

We should bring those capabilities into Claustre's own workbench and session
surfaces.

## Product Design

### 1. Managed-session attention model

Add an explicit attention state for each managed session/thread.

Proposed conceptual states:

- `working`
- `waiting_user`
- `waiting_approval`
- `done_unseen`
- `error_unseen`
- `interrupted_unseen`
- `runtime_unhealthy`

These should be derived from the existing sources of truth:

- DB-backed `ClaudeStatus`
- in-memory paused/waiting detection already present in the TUI
- thread/run/workflow status
- runtime health
- whether the state change has been "seen" in the UI

### 2. Seen vs unseen

Claustre already knows whether a session is paused/waiting at runtime, but it
does not yet treat state transitions as inbox-like events.

We should add "seen" tracking for session/thread attention transitions:

- a session becoming `done`
- a session entering `error`
- a session becoming `interrupted`
- a session needing user input
- a workflow stage waiting for approval

Proposed rule:

- the state becomes seen when the user focuses the relevant thread/session
- unseen state should be reflected in the tab bar, thread list, reviews, and
  dashboard summaries

### 3. Port/runtime presence

Claustre already has a runtime layer and a config-level `ports_dir()`
convention in [`src/config/mod.rs`](/Users/harsha/Code/claustre/src/config/mod.rs).

We should make runtime-backed ports explicit in the workspace:

- visible in the thread header
- visible in the Runtime inspector
- surfaced as badges in the thread/session list
- openable with a single action

This should work for both:

- managed runtime-backed threads
- detected local services associated with a worktree/session

### 4. External sessions

The current scanner is Claude-only and intentionally passive. It parses
`~/.claude/projects/` JSONL state and stores an [`ExternalSession`](/Users/harsha/Code/claustre/src/store/models.rs)
record with:

- project path/name
- model
- branch
- token usage
- timestamps
- JSONL path

That is the correct starting point, but not enough.

We should expand external sessions in phases:

#### Phase A: Better Claude external sessions

- add derived provider state such as `waiting_user` / `busy` where possible
- detect cwd, branch, model, recency, and likely "needs attention" markers
- make them visible alongside managed sessions in the UI

#### Phase B: Codex external sessions

- add a Codex scanner using the already-documented session locations
- parse transcript/session metadata similarly to Claude
- map to the same attention model

#### Phase C: OpenCode external sessions

- add a scanner for OpenCode state if present locally
- continue normalizing into the same UI contract

External sessions should remain observational unless/until we have a safe and
explicit control path.

## UI Changes

### Title bar

Show compact attention chips:

- number of waiting sessions
- number of approval gates
- number of unseen done/error states
- runtime unhealthy count

This should replace vague "something is happening" chrome with actionable
status.

### Sidebar / repository snapshot

Extend the existing repository snapshot to include:

- active managed sessions
- waiting/paused count
- unseen count
- runtime unhealthy count
- external-session count by provider

### Thread list

Each row should include compact badges for:

- provider
- branch
- runtime
- attention state
- unseen marker
- local port badges when available

### Session tabs

Session labels should show an at-a-glance state marker:

- waiting
- approval
- done unseen
- error unseen

This should use the same attention model as the thread list.

### Inspector

Add a lightweight "Session" or "Context" summary to the existing inspector
surfaces rather than introducing a wholly new app mode.

Useful data:

- worktree path
- branch
- provider
- runtime profile
- detected localhost URLs
- external-session origin when applicable

## Architecture Changes

### 1. Add a normalized session attention type

Introduce a Rust type in the TUI/store boundary that all UI surfaces can
consume.

Conceptually:

```rust
enum SessionAttention {
    Working,
    WaitingUser,
    WaitingApproval,
    DoneUnseen,
    ErrorUnseen,
    InterruptedUnseen,
    RuntimeUnhealthy,
}
```

This should be derived rather than manually stored wherever possible.

### 2. Add seen-state persistence

We need lightweight persistence for "user has seen this transition".

Recommended first pass:

- persist seen state per managed thread/session in SQLite
- keep external-session seen state separate, also in SQLite
- do not overload `ExternalSession` itself with transient UI-only fields unless
  we are prepared to update scanner semantics too

Likely schema additions:

- `thread_attention_seen_at`
- `session_attention_seen_at`
- `external_session_seen_at`

### 3. Expand scanner outputs

The scanner contract should move from "a passive Claude snapshot" to "external
session snapshots normalized into Claustre attention signals."

Possible additions to `ExternalSession` or a companion view model:

- provider kind
- attention state
- has_unseen_change
- repo-relative cwd
- port list
- current_status_message

### 4. Event flow

Attention should not wait entirely for the 1-second slow path when the session
host already knows something changed.

Preferred event sources:

- session-host output / lifecycle changes
- existing paused/waiting detection
- runtime health updates
- scanner refresh results

The 1-second polling loop should remain as fallback/reconciliation, not the only
mechanism.

## Execution Plan

### Milestone 1: Managed session attention

- add normalized attention derivation for managed sessions
- add seen/unseen persistence
- show attention badges in thread list, session tabs, and title bar
- mark states as seen when focusing the relevant thread/session

### Milestone 2: Runtime and localhost badges

- detect thread/runtime localhost endpoints
- surface them in header, runtime inspector, and thread/session lists
- add open action from the UI

### Milestone 3: External Claude sessions

- enrich scanner output
- show external sessions in Claustre with provider and attention badges
- make quick focus/jump actions available

### Milestone 4: Codex/OpenCode external sessions

- add additional scanners
- normalize into the same external-session presentation

## Acceptance Criteria

- Users can tell at a glance which managed session needs attention.
- Users can tell whether a state is new/unseen versus already acknowledged.
- Session tabs and thread rows show branch, provider, and attention state
  consistently.
- Runtime-backed threads show localhost URLs/ports where relevant.
- External sessions are visible and useful without confusing them with managed
  Claustre worktrees.
- The new model improves the existing Rust UI instead of adding another
  competing surface.

## Implementation Starting Points

- Attention derivation:
  [`src/tui/app/mod.rs`](/Users/harsha/Code/claustre/src/tui/app/mod.rs)
  [`src/tui/app/data_refresh.rs`](/Users/harsha/Code/claustre/src/tui/app/data_refresh.rs)
- Managed session UI:
  [`src/tui/ui/workbench.rs`](/Users/harsha/Code/claustre/src/tui/ui/workbench.rs)
  [`src/tui/ui/session.rs`](/Users/harsha/Code/claustre/src/tui/ui/session.rs)
  [`src/tui/ui/dashboard.rs`](/Users/harsha/Code/claustre/src/tui/ui/dashboard.rs)
- External sessions:
  [`src/scanner/mod.rs`](/Users/harsha/Code/claustre/src/scanner/mod.rs)
  [`src/store/queries/external_sessions.rs`](/Users/harsha/Code/claustre/src/store/queries/external_sessions.rs)
  [`src/store/models.rs`](/Users/harsha/Code/claustre/src/store/models.rs)
- Runtime/ports:
  [`src/runtime.rs`](/Users/harsha/Code/claustre/src/runtime.rs)
  [`src/config/mod.rs`](/Users/harsha/Code/claustre/src/config/mod.rs)

