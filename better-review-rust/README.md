# Better Review

A terminal PR review interface that tracks reviewed file state locally and marks
GitHub files viewed once the reviewed state catches up to the latest PR content.

```sh
cargo run -- review [PR_OR_URL_OR_BRANCH]
```

Requires the GitHub CLI (`gh`) to be installed and authenticated.
