# WarraqCraft collaboration plan

WarraqCraft is local-first: opening, editing, compiling and keeping project
history must never require an account, a hosted relay or a network
connection. Collaboration is therefore an optional layer over a normal
project folder, not a replacement for the folder model.

## Goals

- Preserve a normal `.tex` project on disk as the source of truth.
- Keep offline work, atomic saves, snapshots and recovery unchanged.
- Let users collaborate without forcing WarraqCraft-specific hosting.
- Avoid silent merge behaviour; every conflict state must be explicit.
- Keep the same command surface reachable through UI, CLI, control and MCP.

## Option A — Git-based collaboration (recommended first step)

A project already maps naturally to a Git repository. This path adds UX and
engine commands around the standard workflow rather than a new service.

### Proposed commands

- `repo.status` — current branch, upstream, dirty files and ahead/behind.
- `repo.pull` / `repo.push` — explicit user-triggered operations.
- `repo.commit` — staged project snapshot with a user-authored message.
- `repo.branches` — list/create/switch, guarded against dirty buffers.
- `repo.conflicts` — list conflicted paths and open merge guidance.
- `repo.history` — map local snapshot history onto repository history.

### UX

- Branch and dirty-state indicator in the project/status area.
- Pull/push actions only through explicit buttons or commands.
- A dirty untitled buffer blocks switching branches until saved or
  intentionally discarded.
- Remote URLs and credentials are managed by the user's Git tooling;
  WarraqCraft does not store credentials in preferences.

### Strengths

- No account, relay or new trust boundary.
- Uses proven review, branching and CI workflows.
- Works with GitHub, GitLab, Forgejo or a private remote.

### Limits

- Not real-time co-editing.
- Users still resolve normal Git conflicts.

## Option B — optional live session sync (Overleaf-like)

A later phase can add opt-in live sessions on top of the same project
folder. This should be treated as a separate service/integration, not as a
prerequisite for phase 1.

### Minimal model

- A project owner starts or joins a session by URL/token.
- A relay routes encrypted document operations to participants.
- Each participant keeps a local project folder and can continue offline.
- Presence/cursors are display metadata, never part of the `.tex` source.
- Compile still happens locally; there is no required cloud compile queue.

### Technical candidates

- CRDT edits (Yjs-style semantics implemented in Rust) for live text.
- Simpler OT/lock-based editing if CRDT complexity is not justified.
- End-to-end encrypted sync so the relay cannot read document contents.
- Self-hosted and managed relay options, both disabled by default.

### Hard requirements

- Explicit opt-in; no background connection by default.
- Session records never become the only copy of a document.
- Rejoin/reconcile must preserve local history and never overwrite a
  dirty buffer silently.
- Permissions map onto engine commands: read, comment, edit, compile,
  administer.
- The protocol and safety model must be designed before implementation.

## Decision

Ship phase 1 without a bespoke collaboration service. Document Git-based
collaboration, then implement Git-aware status and guarded operations as
the first online-work feature. Keep the CRDT/relay option as a separate
design milestone after the file/project model and command permissions are
stable.
