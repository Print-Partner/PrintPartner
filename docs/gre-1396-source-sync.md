# GRE-1396: Source snapshot cap, duplicate names, and part slugs

These rules apply to the Node server. The Rust naming preview (`pp-storage`
`catalog/naming/preview.rs`) follows the same slug and quantity rules.

## Snapshot cap

A Source snapshot may store up to 2 GiB (2147483648 bytes) by default. The cap
was 1 GiB, which refused DW-Tas/A4Tv2 (about 1.37 GB). To change it, set
`PP_SOURCE_SNAPSHOT_MAX_BYTES` to a whole number of bytes and restart the
server. Each sync reads the value, so a bad value fails that sync with
`PP_SOURCE_SNAPSHOT_MAX_BYTES must be a whole number of bytes above 0` and
leaves no candidate folder. The tenant disk quota and the documentation budget
still apply.

## Duplicate Sources

`propose_add_source` refuses a new Source only when one of these holds:

- An existing Source has the same name after folding case and removing spaces,
  `_` and `-`. `nevermore stealthmax` matches `Nevermore-StealthMax`.
- An existing Source was added from the same repository. GitHub URLs compare
  by owner and repository in any form (`https`, `.git`, `git@github.com:`, or a
  `/tree/<branch>` link). Other URLs compare as text without trailing `/`,
  ignoring case.

A name that only contains another one is a new Source: `A4Tv2` next to `A4T`,
or `Nevermore-StealthMax-V2` next to `Nevermore-StealthMax`. Apply checks
again, so a proposal made before the duplicate existed is refused. Tools that
look a Source up by name still use the looser `sourceByName` match.

## Part slugs and quantities

- The role markers `[a]`, `[c]` and `[o]` (or a profile's own markers) are
  removed anywhere in the file name, with one separator (space, `_` or `-`)
  next to them. When words remain on both sides, one separator stays between
  them: `[a]_belt_clip` is `belt_clip`, `FlowChamber_BPM[a]` is
  `FlowChamber_BPM`, and `Duct_[c]_Left` is `Duct_Left`.
- `.3mf` names parse like `.stl` names. The extension is dropped from the
  slug, and the quantity regex reads `Fan_Spring_x2.3mf` as
  `Fan_Spring_x2.stl`, so profiles saved with a `\.stl$` regex count it too.

Two files whose names differ only by a marker now share a slug. In one Plan
they report a merge conflict, which is what marker stripping is for: an addon's
`[a]_foo.stl` replacing a base `foo.stl`. Slugs are not stored. Plan parts
are keyed by path, so the new slugs change only conflict status, not part
identity or Required units.

GitHub sync still skips `.3mf` files. Importing them as parts is a separate
ticket.
