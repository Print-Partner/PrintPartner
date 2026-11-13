# Library and Sources UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Sources | `GET /sources`, `/sources/activity`, naming, import rules | Exists in Rust behind `desktop_rebuild`. The generated client's `listSources` and `listSourceActivity` read `/api/v1/sources` and `/api/v1/sources/activity` (GRE-1364). Library (`/library`) lists each Source with its revision, sync state and newest activity, and the Build Sources tab shows each layer's Source, its current revision, and the revision the Accepted Plan used, read from the Build's Plan freshness |
| Add a Source | `POST /api/v1/sources` (`createSource`) | Exists in Rust behind `desktop_rebuild` (GRE-1490). Library's Add Source dialog uses it (GRE-1513) |
| GitHub branches and tags | `GET /sources/github-branches?url=`, `GET /sources/github-tags?url=` | Exists in Rust behind `desktop_rebuild` (GRE-429 PR 1), not yet in the generated client. `github-branches` answers `owner`, `repo`, `default_branch`, `url_branch` and every branch; `github-tags` answers `owner`, `repo` and every tag. Both send the token saved with `PUT /settings/github-pat` and read at most 100 pages of 100. A missing `url` or one that is not a GitHub repository is 400, and GitHub's 404 is 404. Any other GitHub failure, a request with no answer in 30 seconds, or a listing not done in 60 seconds is 502; a refused token has `code` `github_token_rejected`, and GitHub's 403 or 429 has `github_refused` |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 14 | Library and Sources tab | Manage Sources |

## Add Source

Library has an **Add Source** button under its heading in every state, the empty state included, (`web/apps/ui/src/library/AddSourceDialog.tsx`). It opens a dialog:

- **Name**, and a kind: **ZIP archive** (`source_kind: "archive"`) or **GitHub repository** (`source_kind: "github"`). GitHub also asks for the repository URL and a branch, a text field that starts as `main`.
- **Create** is disabled while the name is empty, while GitHub is chosen without a URL, and while the core is offline; the reason shows beside it. **Cancel** sends nothing.
- Create sends `createSource` once and never resends it by itself. On success the dialog closes, the Sources are read again, and the new row shows "No revision yet" and "Never synced".
- A 4xx is the core's refusal, such as a name already in use: the dialog stays open with the core's reason. A 5xx or a lost answer may come after the core stored the Source, so the dialog says the add could not be confirmed and reads the Sources again. Pressing Create again with the same name cannot add a second Source, because the core refuses a name already in use. That check is case-sensitive, so a differently capitalized name is a new Source.

Uploading a ZIP, the branch list and sync are later parts of GRE-1365.
