# Rulings that apply to this grading (2026-11-09)

These rulings were made before the reruns and apply to every answer.

- Q1, commit on outcome: the key's fact that `execute` commits only on `Applied`, `Existing` or `AlreadyApplied` is not required. An answer that names `execute`, the immediate transaction, `publish_in_transaction` and the atomicity test is correct.
- Q3, GATED: the `GATED` step applies only to an edge that must be `desktop_rebuild`-only. The question does not say the edge is desktop-only, so the general add-edge steps are required. The crate map in `docs/architecture.md` is still required, and an answer that names only the ADR 0007 table is incorrect.
