# Library and Sources UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Sources | `GET /sources`, `/sources/activity`, naming, import rules | Exists in Rust behind `desktop_rebuild`. The generated client's `listSources` and `listSourceActivity` read `/api/v1/sources` and `/api/v1/sources/activity` (GRE-1364). Library (`/library`) lists each Source with its revision, sync state and newest activity, and the Build Sources tab shows each layer's Source, its current revision, and the revision the Accepted Plan used, read from the Build's Plan freshness |
| Add a Source | `POST /api/v1/sources` (`createSource`) | Exists in Rust behind `desktop_rebuild` (GRE-1490). Library's Add Source dialog uses it (GRE-1513) |
| Upload a ZIP | `POST /api/v1/sources/{id}/upload-zip` (`uploadSourceZip`) | Exists in Rust (GRE-1444). The body is `multipart/form-data` with no request DTO. The 202 answer is `{job_id}` (`SourceUploadAccepted`). Library posts one ZIP and follows the job (GRE-1725) |
| GitHub branches and tags | `GET /sources/github-branches?url=`, `GET /sources/github-tags?url=` | Exists in Rust behind `desktop_rebuild` (GRE-429 PR 1). `listGithubBranches` is in the generated client (GRE-1728); `github-tags` is not. `github-branches` answers `owner`, `repo`, `default_branch`, `url_branch` and every branch; `github-tags` answers `owner`, `repo` and every tag. Both send the token saved with `PUT /settings/github-pat` and read at most 100 pages of 100. A missing `url` or one that is not a GitHub repository is 400, and GitHub's 404 is 404. Any other GitHub failure, a request with no answer in 30 seconds, or a listing not done in 60 seconds is 502; a refused token has `code` `github_token_rejected`, and GitHub's 403 or 429 has `github_refused` |
| Sync a GitHub Source | `POST /api/v1/jobs/sync` (`startSourceSync`) | In the generated client (GRE-1728). The body is `{ "project_ids": [<id>] }` and the `202` answer is `{ "job_id" }`. Library starts one sync after creating a GitHub Source, and each `github` row has Sync |
| Source files | `GET /api/v1/sources/{id}/files` (`listSourceFiles`) | In the generated client (GRE-1731). `revision_id` is null and `files` is empty when the Source has no revision. Each file has `path`, `kind` (`stl`, `model`, `document`, `image`, `other`) and `size_bytes`, sorted by path. Selecting a Source opens `/library/{id}` and shows that list grouped by folder |
| STL thumbnail | `GET /api/v1/sources/{id}/thumbnail?path=` | A 256 × 256 PNG of one STL in the current revision (GRE-1520). Not a generated-client route. Library, the Builds list and a Build's Sources tab request it as an image once the row is in the viewport. A failed response is a placeholder. The PNG is not tinted by filament colour |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 14 | Library and Sources tab | Manage Sources |

## Add Source

Library has an **Add Source** button under its heading in every state, the empty state included, (`web/apps/ui/src/library/AddSourceDialog.tsx`). It opens a dialog:

- **Name**, and a kind: **ZIP archive** (`source_kind: "archive"`) or **GitHub repository** (`source_kind: "github"`). GitHub also asks for the repository URL and a branch, a text field that starts as `main`.
- Leaving the URL field reads `listGithubBranches`. The branch field becomes a list with the repository's default branch selected. If that read fails, the text field stays and the core's reason is shown under it.
- **Create** is disabled while the name is empty, while GitHub is chosen without a URL, and while the core is offline; the reason shows beside it. **Cancel** sends nothing.
- Create sends `createSource` once and never resends it by itself. On success the dialog closes and the Sources are read again. A GitHub Source then starts one `startSourceSync` for its id. The new row says **Syncing** until that job ends.
- A 4xx on create is the core's refusal, such as a name already in use: the dialog stays open with the core's reason, and no sync is started. A 5xx or a lost answer may come after the core stored the Source, so the dialog says the add could not be confirmed and reads the Sources again. Pressing Create again with the same name cannot add a second Source, because the core refuses a name already in use. That check is case-sensitive, so a differently capitalized name is a new Source. A refused or lost sync does not start another one; the row shows the reason and Sync stays available.

## Upload a ZIP

Every row whose `source_kind` is not `github` has **Upload ZIP** (`web/apps/ui/src/library/ZipUpload.tsx`). It posts one `.zip` through `fetchCore` and checks the 202 body with the generated `uploadSourceZip` schema.

- The row says **Uploading** until the core answers 202. While the row is uploading, or its job is `pending` or `running`, Upload ZIP is disabled.
- From the 202 the row says **Importing**, with a percentage when the job reports `progress`. The UI reads `readJob` at once, then every 2 seconds while the job is `pending` or `running`, and again 2 seconds after a read that fails for any reason other than 404. It does not poll a finished or forgotten job, and it does not poll because the window focused or the browser reconnected.
- `done`: the row shows the new revision, and every query under `["core", "sources"]` is read again. When `result.warnings` is a non-empty list of strings, those strings are listed under the row.
- `error`: the row says **Import failed** with the job's `error`. The Revision cell still shows the revision the Source had.
- A refusal (400, 404, 409, 411, 413, 503, 507, and any other error status) shows the core's reason on the row. No job is followed.
- The browser stores each Source's job id in `localStorage` under `pp.library.jobs` as soon as the 202 is read. After a reload, or after a failed job read while the core is down, the row follows that same job. A job the core no longer knows (404) is removed from `pp.library.jobs` and the Sources are read again.
- The UI does not upload again on reload, on reconnect, or after a lost answer. Choosing the file again is a new upload.

## Sync

Every row whose `source_kind` is `github` has **Sync** (`web/apps/ui/src/library/ZipUpload.tsx`). It posts `{ "project_ids": [id] }` once. While that job is `pending` or `running`, Sync is disabled and the row says **Syncing**, with a percentage when the job reports `progress`. The same job read used for a ZIP import follows it: immediately, then every 2 seconds while it is `pending` or `running`, and again 2 seconds after a read that fails for any reason other than 404.

`done` reads the Source queries again. The Revision cell shows the branch or tag at the 7-character commit, and the Sync cell shows when it last synced. `error` says **Sync failed** with the job's `error` and keeps the revision already on the row. A `done` job whose `result.failures` names this Source also says **Sync failed** with that failure's text.

The job id is written to `localStorage` key `pp.library.jobs` as soon as the 202 is read, the same map a ZIP import uses. A reload follows that same job and does not start another sync. A 404 removes it. Sync and the create flow above are the only ways a sync starts.

## Source files

The Source's name is a link to `/library/{id}` (`web/apps/ui/src/library/SourceFiles.tsx`). That page lists the current revision's files from `listSourceFiles`, grouped by folder, with each file's name and size. Files at the revision root are under **Top level**. An STL has a 64 px thumbnail from `GET /api/v1/sources/{id}/thumbnail?path=`; the image is requested when the row enters the viewport, and a failed response shows **No preview** instead of a broken image. A Source with no revision says **No files yet.** Clicking the selected name returns to `/library`.

A Build's Sources tab (`/builds/:id/sources`) lists each of that Build's Sources the same way, in layer order, one section per Source. A Source used on two layers is listed once.

A finished import or sync reads every query under `["core", "sources"]` again, including that Source's file list.
