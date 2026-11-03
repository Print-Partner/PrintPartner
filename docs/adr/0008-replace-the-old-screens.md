# Replace the old screens instead of porting them

Status: accepted on 2026-11-02 by Chad. Supersedes parity-first screen delivery and
the UI v11 sequence, not required data, export, or protocol compatibility.

Build the replacement UI only against the Rust API using the
[accepted UI target](../product/ui-ux-target.md). Freeze old screens to correctness
and data-safety maintenance, then delete them after replacement acceptance and
cutover. Porting old screens before replacing them would pay for UI wiring twice.
The cost is that the Rust product is not shippable until its new screens work.
Preserve useful domain behavior with golden tests, not old-screen parity.

Whole-runtime cutover was a separate choice Chad made in a later answer on
2026-11-02, with explicit rollback limits. [ADR 0010](0010-whole-runtime-cutover.md)
records it. It was not accepted implicitly with screen replacement.
The existing per-route strangler is working code that becomes unused by the new
product. Removing it is scheduled scope, not a defect fix.
