# Print Partner community manifests

This folder holds **community-submitted** kit manifests for STL repositories that do not ship an owner-authored `print-partner.manifest.yaml`.

## Authoritative owner manifest (preferred)

Project maintainers should add **`print-partner.manifest.yaml`** at the **root** of their STL repository. Print Partner loads it automatically on **Sources → Sync**. Power users can edit repo YAML through the manifest API or the community pull-request flow below. Each API save publishes a new immutable Source revision. It does not modify the synced upstream files.

## Submit a community manifest

1. Fork [PrintPartner](https://github.com/Print-Partner/PrintPartner).
2. Add `manifests/community/{slug}/manifest.yaml` and `meta.yaml` (submitter, `repo_url`, branch).
   A slug is 1 to 64 lowercase letters, digits, dots, hyphens or underscores, and starts with a letter or digit. Example: `ldo-2.4-sb-tap`.
   The stems `kit-catalog` and `registry-index` are reserved for application data and cannot be slugs in registry submissions.
3. Add an entry to `manifests/registry/index.yaml` with `status: proposed`.
4. Open a **pull request**. CI reads `version` and validates against the matching
   v1 or v2 JSON Schema.
5. A linked **GitHub Issue** is used for discussion; use 👍 / 👎 reactions or maintainer checklist for consensus.
6. When approved, maintainers set `status: approved` in the index.

## Voting

- One active **approved** entry per `target_repo` + branch unless `variant` is set in `meta.yaml`.
- Deprecate by setting `status: deprecated` and commenting on the issue.

## App import

Users pick an approved manifest in the community registry or rely on the repo-root file after sync. Maintainers export a PR bundle via the manifest registry API when submitting to this repository.

A Source stores its community manifest slug in `manifest_community_slug`. An empty value means no community manifest. The server refuses a slug that breaks the grammar above or uses `kit-catalog`: kit bundle imports return HTTP 400, and assistant Source updates return an error. The server ignores and logs an invalid slug stored before this check.

## Example v2 stack export

A plan with base + addons exports as source references (URLs + rules), not embedded STLs:

```yaml
# Example: a vendor kit stack (community manifests live under manifests/community/<slug>/)
base:
  source: Example-Printer
  url: https://github.com/ExampleOrg/Example-Printer
addons:
  - source: Example-Toolhead
    url: https://github.com/ExampleOrg/Example-Toolhead
  - source: Example-Probe
    url: https://github.com/ExampleOrg/Example-Probe
selections:
  toolhead: example_toolhead
  probe: example_probe
```

## Option-group selections

Manifest v2 stores a `pick_one` selection as a string. It stores `pick_any` and `pick_n` selections as YAML lists, including lists with one id. Do not join several ids into one string.

| Rule | Selection value | Count |
|------|-----------------|-------|
| `pick_one` | One variant id string | Zero or one while editing |
| `pick_any` | A list of variant ids | Any number, limited by `min` or `max` when present |
| `pick_n` | A list of variant ids | The inclusive `min` and `max` range |

Lists must contain unique, non-empty variant ids. Omit a group from `selections` when it has no selections.

The runtime reads an existing scalar value in a multi-choice group as a one-item selection. New clients write a list for every multi-choice group.

```yaml
option_groups:
  extras:
    rule: pick_n
    min: 1
    max: 2
    variants:
      - id: skirts
        parts: ["skirts/**"]
      - id: panels
        parts: ["panels/**"]
selections:
  extras:
    - skirts
    - panels
```

Print Partner includes the union of the selected variants' `parts`. A selected variant's `excludes` patterns take precedence. A selection below `min` or above `max` includes no parts from that group until its count returns to the allowed range.

See `docs/kit-catalog.yaml` → `stack_presets` for reference preset ids.

## Validation and embedded copies

Files under `manifests/community/` and `manifests/registry/` are authoritative.
The server copies under `web/apps/server/src/data/manifests/` are generated
runtime assets; CI rejects drift.
The validator rejects registry slugs that break the grammar or use either reserved stem before syncing any embedded copies.

```bash
python -m pip install -r manifests/requirements.txt
python manifests/scripts/validate.py
# After changing a canonical registry or manifest:
python manifests/scripts/validate.py --sync-embedded
```

## Canonical paths for community manifests

Community and registry manifests must express rules as **relative path globs** from each repo root (same normalization as scan `match_key`):

| Pattern | Example | Use |
|---------|---------|-----|
| Folder glob | `PrintedParts/**` | Required/optional folder, variant membership |
| Nested folder | `Stealthburner/**` | Toolhead variant in addon repo |
| Flexible match | `**/Tap/**` | Probe variant when folder name is stable |
| File glob | `**/frame_*.stl` | Required frame parts |

**Do not** use absolute paths or plan-private ids in `parts[].match` or `variants[].parts`.

Align folder names with [the shipped path hints](../web/apps/server/src/data/path-hints.yaml) where possible so import-rule suggestions and CI review scoring stay consistent. Shared category ids (e.g. `toolhead`, `probe`) merge across repos; variants are distinguished by path globs per repo.
