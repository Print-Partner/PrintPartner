# GRE-1309 signpost prose audit

Report-only pass of the `unslop` skill over the six navigation signposts at `77e4be151`, before any edit.
Line numbers are from that commit.

Files: `docs/families.md`, `rust/AGENTS.md`, `rust/crates/pp-compat/AGENTS.md`, `web/apps/server/AGENTS.md`, `web/apps/web/AGENTS.md`, `web/apps/ui/AGENTS.md`.

## Scanner results

| Scanner | Result |
| --- | --- |
| `banned_phrase_scan.py` | 0 hard, 0 soft in all five `AGENTS.md` files and in the `docs/families.md` intro (lines 1-16) |
| `structure_scan.py` | 1 soft flag each in `rust/AGENTS.md` (`sentence_burstiness` 0.475) and `web/apps/server/AGENTS.md` (0.438); none elsewhere |
| `silhouette_scan.py` | 0 flags, penalty 0.0 in all six |
| Whole-file `docs/families.md` | All three scanners declined it as non-English because the table is mostly paths. The intro was scanned on its own. |

The scanners found nothing to fix. Every row below comes from reading each sentence.

## Findings

Action is one of: **Fix** (confirmed defect, repaired in the next commit), **Leave** (confirmed defect, but the repair would break a constraint of this task), **Protect** (not a defect in this context).

| File | Line | Pattern | Action |
| --- | --- | --- | --- |
| `docs/families.md` | 5 | Repeated word: "Architecture owns target scope and runtime ownership". "Owns" is also the term for which runtime writes a family, so the sentence uses it in two senses. | Fix: "owns" becomes "defines". |
| `docs/families.md` | 5-6 | Inanimate agency: "These paths do not claim that a whole family ships". A path cannot claim anything. | Fix: "A path here does not mean that a whole family ships." |
| `docs/families.md` | 6-7 | Category slip: "cover existing tests, including partial implementations". An implementation is not a test. | Fix: "including tests of partial implementations". |
| `docs/families.md` | 7 | Elliptical claim: "they do not prove deferred work". It does not say what about the work is unproven. | Fix: "they prove nothing about deferred work". |
| `docs/families.md` | 16 | Agentless passive hedge: "No Rust family coverage is claimed for those rows". The table already states "None; no Rust coverage" for the same rows. | Fix: "Those rows have no Rust coverage." |
| `rust/AGENTS.md` | 5 | Vague link label: "the authority page" hides the name of the page it points to. | Leave: link text and target stay byte-for-byte. |
| `rust/crates/pp-compat/AGENTS.md` | 5 | Vague link label: "the authority page". | Leave: same reason. |
| `web/apps/server/AGENTS.md` | 5 | Vague link label: "the authority page". | Leave: same reason. |
| `web/apps/web/AGENTS.md` | 5 | Vague link label: "the authority page". | Leave: same reason. |
| `web/apps/ui/AGENTS.md` | 5 | Vague link label: "the authority page". | Leave: same reason. |
| `web/apps/ui/AGENTS.md` | 16 | Scope ambiguity: "old UI and domain packages" reads as either "the old UI, and domain packages" or "old UI packages and old domain packages". | Leave: picking a reading adds a fact the file does not state. The owner should decide. |
| `web/apps/server/AGENTS.md` | 3 | Vague imperative: "Maintain correctness and data safety here" names no action. | Leave: the plain form ("only correctness and data-safety fixes") is a stronger rule than the file states. |
| `rust/crates/pp-compat/AGENTS.md` | 15 | Redundant modifier: "add new domain operations". | Protect: a matter of taste, and the line is a rule. |
| `rust/crates/pp-compat/AGENTS.md` | 16 | Noun stack: "replacement acceptance and cutover approval". | Protect: both are terms from `docs/adr/0008-replace-the-old-screens.md` and `docs/architecture.md`. |
| `rust/AGENTS.md` | 3 | Opaque phrase: "`pp-core` composes operation owners". | Protect: "composes" and "operation owner" are terms from `docs/architecture.md` and `docs/development/agent-workflow.md`. |
| All five `AGENTS.md` | 3 | Sentence fragment as status: "Active.", "Retiring.", "Frozen, retiring at whole-runtime cutover.", "Frozen to correctness and data-safety fixes." | Protect: a status label, normal for a signpost. |
| All five `AGENTS.md` | 3 | Repeated verb: "owns", 9 uses. | Protect: the domain term for write ownership in `docs/architecture.md`. |
| All five `AGENTS.md` | 15-18 | Anaphora: 14 bullets open with "Do not". | Protect: a rule list. The wording carries the force of each rule. |
| `rust/AGENTS.md`, `web/apps/server/AGENTS.md` | whole file | Uniform sentence length (the `sentence_burstiness` soft flag). | Protect: advisory for reference prose, and nothing else corroborates it. |
| `docs/families.md` | 15 | Negative contrast: "this checks paths, not behaviour". | Protect: it states the real limit of the check. |
| `docs/families.md` | 18-38 | Repeated cell text: "None; no Rust route for this slice", "None; Rust implementation deferred", "None; no Rust coverage". | Protect: `scripts/check-family-index.py` reads the `None;` prefix, and table rows stay byte-for-byte. |

## Count by pattern

| Pattern | Rows | Fix | Leave | Protect |
| --- | --- | --- | --- | --- |
| Vague link label | 5 | 0 | 5 | 0 |
| Repeated word or verb | 2 | 1 | 0 | 1 |
| Inanimate agency | 1 | 1 | 0 | 0 |
| Category slip | 1 | 1 | 0 | 0 |
| Elliptical claim | 1 | 1 | 0 | 0 |
| Agentless passive hedge | 1 | 1 | 0 | 0 |
| Scope ambiguity | 1 | 0 | 1 | 0 |
| Vague imperative | 1 | 0 | 1 | 0 |
| Redundant modifier | 1 | 0 | 0 | 1 |
| Noun stack | 1 | 0 | 0 | 1 |
| Opaque phrase | 1 | 0 | 0 | 1 |
| Status fragment | 1 | 0 | 0 | 1 |
| Anaphora | 1 | 0 | 0 | 1 |
| Uniform sentence length | 1 | 0 | 0 | 1 |
| Negative contrast | 1 | 0 | 0 | 1 |
| Repeated cell text | 1 | 0 | 0 | 1 |
| **Total** | **21** | **5** | **7** | **9** |

All five fixes are in `docs/families.md`. No `AGENTS.md` file has a defect that this task's constraints allow repairing.
