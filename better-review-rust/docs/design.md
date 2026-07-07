# Better Review Design

## Goal

Better Review is a Rust terminal interface for reviewing GitHub pull requests in
small hunks while keeping local reviewed state across PR updates. The initial
backend uses the `gh` command-line tool.

## Reviewed-State Model

The central artifact is a local reviewed-state file for each PR file.

On first review, the reviewed-state file is initialized from the PR base:

- Modified file: base version of the file.
- Added file: empty file.
- Deleted file: base version of the file; the latest PR content is empty.
- Renamed file: base content from the previous path; reviewed as the current path.

The UI always computes a normal diff from the reviewed-state file to the latest
PR head content. There is no diff-of-diffs model. Accepting a hunk applies that
hunk into the reviewed-state file. When the reviewed-state file exactly matches
the latest PR head content, Better Review marks the file viewed in GitHub.

If the PR changes later, the latest head content changes. The reviewed-state file
remains the user's last accepted state, so the next review shows only the
remaining delta from that state to the latest file.

## Storage

State is stored as literal files under the user's platform data directory:

```text
better-review/
  github.com/
    owner/
      repo/
        pr-123/
          manifest.json
          reviewed/
            path/to/file.rs
```

`manifest.json` stores PR metadata, file status, previous paths, base/head OIDs,
and content hashes. File content is stored as ordinary files so it can be
inspected and recovered without a database.

## GitHub Integration

Better Review shells out to `gh`:

- `gh pr view [PR] --json ...` resolves PR metadata and file status.
- `gh api graphql` fetches file blobs at base and head refs.
- `gh api graphql` runs `markFileAsViewed` after a file catches up.

The CLI accepts an optional PR number, URL, or branch. If omitted, `gh` resolves
the PR for the current branch.

## Terminal UI

The TUI shows a file list and diff pane.

Keys:

- `j/k` or arrows: move selection.
- `n/p`: next or previous hunk.
- `a`: accept selected hunk.
- `A`: accept all hunks in the current file.
- `w`: toggle diff wrapping versus horizontal scrolling.
- `h/l` or left/right arrows: adjust horizontal scroll offset in scroll mode.
- `r`: refresh from GitHub.
- `q`: quit.

The TUI includes a log pane showing recent startup, refresh, and review actions.
Startup progress is also written to stderr before entering the alternate-screen
TUI so slow GitHub fetches are visible.

After the initial load, slow GitHub work runs on a background worker thread.
Refreshing file contents and marking files viewed in GitHub are queued as jobs so
the TUI keeps rendering and accepting navigation input while those requests are
pending. Local reviewed-state writes still happen synchronously before the UI
reports a hunk or file accepted.

Global user preferences are stored in the platform config directory as
`prefs.json`. The initial preferences are diff wrapping enabled and horizontal
scroll offset `0`.

## V1 Non-Goals

- Posting GitHub review comments.
- Binary file review.
- Unmarking GitHub files viewed.
- Fuzzy moved-code matching. The reviewed-state model makes this unnecessary for
  the initial workflow.
