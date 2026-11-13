# Library and Sources UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Sources | `GET /sources`, `/sources/activity`, naming, import rules | Exists in Rust behind `desktop_rebuild`. The generated client's `listSources` and `listSourceActivity` read `/api/v1/sources` and `/api/v1/sources/activity` (GRE-1364). Library (`/library`) lists each Source with its revision, sync state and newest activity, and the Build Sources tab shows each layer's Source, its current revision, and the revision the Accepted Plan used, read from the Build's Plan freshness. Both are read only |
| GitHub branches and tags | `GET /sources/github-branches?url=`, `GET /sources/github-tags?url=` | Exists in Rust behind `desktop_rebuild` (GRE-429 PR 1), not yet in the generated client. `github-branches` answers `owner`, `repo`, `default_branch`, `url_branch` and every branch; `github-tags` answers `owner`, `repo` and every tag. Both send the token saved with `PUT /settings/github-pat` and read at most 100 pages of 100. A missing `url` or one that is not a GitHub repository is 400, and GitHub's 404 is 404. Any other GitHub failure, a request with no answer in 30 seconds, or a listing not done in 60 seconds is 502; a refused token has `code` `github_token_rejected`, and GitHub's 403 or 429 has `github_refused` |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 14 | Library and Sources tab | Manage Sources |
