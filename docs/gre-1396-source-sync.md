# GRE-1396: Source snapshot cap, duplicate names, and part slugs

These rules apply to the Node server. In Rust, the naming preview
(`catalog/naming/preview.rs`) and working-draft preparation
(`working_drafts/preparation.rs`) call the same `naming::strip_marker` and
`naming::quantity_capture`, so the slug and quantity the preview shows are the
ones drafts use.

## Snapshot cap

A Source snapshot may store up to 2 GiB (2147483648 bytes) by default. The cap
was 1 GiB, which refused DW-Tas/A4Tv2 (about 1.37 GB). To change it, set
`PP_SOURCE_SNAPSHOT_MAX_BYTES` to a whole number of bytes and restart the
server. The cap limits what a sync writes, so a sync reads it only when it
writes a new snapshot. A bad value fails that sync with
`PP_SOURCE_SNAPSHOT_MAX_BYTES must be a whole number of bytes above 0` and
leaves no candidate folder. A sync whose upstream revision is already stored
reuses that snapshot and does not read the cap, so a bad value does not fail
it. The tenant disk quota and the documentation budget still apply.

## Duplicate Sources

`propose_add_source` refuses a new Source only when one of these holds:

- An existing Source has the same name after folding case and removing spaces,
  `_` and `-`. `nevermore stealthmax` matches `Nevermore-StealthMax`. A name
  made only of those separators folds to an empty name, so `___` matches
  `---`.
- An existing Source was added from the same repository. GitHub URLs compare
  by owner and repository in any form (`https`, `.git`, `git@github.com:`, or a
  `/tree/<branch>` link). Other URLs compare as text without trailing `/`,
  ignoring case.

A name that only contains another one is a new Source: `A4Tv2` next to `A4T`,
or `Nevermore-StealthMax-V2` next to `Nevermore-StealthMax`. Apply checks
again, so a proposal made before the duplicate existed is refused. Tools that
look a Source up by name still use the looser `sourceByName` match.

## Part slugs and quantities

- The role markers `[a]`, `[c]` and `[o]` are removed anywhere in the file
  name, with one separator (space, `_` or `-`) next to them. When words remain
  on both sides, one separator stays between them: `[a]_belt_clip` is
  `belt_clip`, `FlowChamber_BPM[a]` is `FlowChamber_BPM`, and `Duct_[c]_Left`
  is `Duct_Left`. Removal repeats until none is left, so a run such as
  `[a]_[a]_belt_clip` or `belt_clip_[a]_[a]` is also `belt_clip`.
- This applies to any marker that starts and ends with a symbol. A custom
  marker that starts or ends with a letter or digit, such as `A`, is removed
  only as a prefix, so `gear` stays `gear` and `A_gear` becomes `gear`.
- `.3mf` names parse like `.stl` names, and the extension is dropped from the
  slug.
- The quantity regex reads the file name as written, so a saved regex keeps
  its meaning: `_x([0-9]+)\.3mf$` gives 2 for `Duct_x2.3mf`, and its slug is
  `Duct`. Only when the regex does not match a `.3mf` name is the name read
  again as the `.stl` of the same name. The default `_x([0-9]+)\.stl$` then
  gives 2 for `Fan_Spring_x2.3mf`. A regex that matches the `.3mf` name with
  an empty group gives the default quantity, as before.
- When the slug's quantity suffix is removed, a `.3mf` slug accepts a regex
  anchored on `\.3mf$` or `\.stl$`. An `.stl` slug accepts only `\.stl$`, as
  before.

Two files whose names differ only by a marker now share a slug. In one Plan
they report a merge conflict, which is what marker stripping is for: an addon's
`[a]_foo.stl` replacing a base `foo.stl`. Slugs are not stored. Plan parts
are keyed by path, so the new slugs change only conflict status, not part
identity or Required units.

GitHub sync still skips `.3mf` files. GRE-1776 tracks importing them as parts.
