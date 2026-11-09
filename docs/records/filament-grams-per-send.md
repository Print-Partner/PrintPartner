# Filament grams per send (GRE-1165, part a: GRE-1237)

Schema step 46 adds the Rust-only table `print_link_grams`, one row per tenant and
Printer Checkoff link. A link is one send of a file to a printer. Its row holds
the grams that send uses (`grams`, positive), the number of Checkoff units the
send prints (`units`, positive), where the grams came from (`basis`:
`estimated` or `measured`), the slicer's grams per millimetre of filament
(`grams_per_mm`, when the file states both), and the length the printer
reported using at finish (`measured_mm`). Step 46 takes a `pre-schema46.db` copy like every
step through `LAST_COPIED_STEP`, and the table is validated against
`print_grams/schema.sql` like the other side tables. Node has no such table and
never writes it.

The owner decided on 2026-11-04 to record the slicer's estimate at send and
replace it with the measured value when the print finishes. Printers report
millimetres, and the staged file is deleted after upload, so the conversion
factor is kept at send (Alfala, 2026-11-07):

- `pp_storage::print_grams::record_estimate` writes the estimate and its
  `grams_per_mm`. A later estimate replaces it only until a length is measured.
- `pp_storage::print_grams::record_measured` stores `measured_mm`. With a
  `grams_per_mm`, the grams become `measured_mm × grams_per_mm` and the basis
  `measured` (`Recorded`). Without one, the estimate stays (`NoFactor`). A link
  with no estimate is left alone (`NoEstimate`).

The send queue's upload worker calls `record_estimate` when an upload with
Checkoff units succeeds (GRE-1238, see [send-queue.md](../send-queue.md)).

The measured value (GRE-1165, part c) comes from `pp_core::measured_grams`, an
observer on the Moonraker and PrusaLink status watchers' ordered transitions. When a host
goes to `complete` straight from `printing` or `paused` (an `offline` or
`unknown` reading in between does not break the run), it asks the host once
for the finished print's file name and filament length in one read
(`HostReads::finished_filament`: Moonraker `print_stats` `state`, `filename`
and `filament_used`; PrusaLink's job `state`, `file` and `consumed_material`,
read as millimetres). The length counts only when that read still reports the
print finished (`complete` or `FINISHED`), so a slow observer never records the
next print's usage against this one. When the status named a file, the read
must name the same base name. When it named none, the read's file name is used:
PrusaLink's `/api/v1/status` job carries no `file`, so a PrusaLink finish is
named by `/api/v1/job` alone. Storage then picks the newest matching Checkoff
link of that host and calls `record_measured` for it in one write. A link
matches when it is `watching`, `awaiting_verify` or `verified`, was created no
later than the moment the observer saw the finish (so a send of the same file
name uploaded after it never takes its length), and its `filename` or
`remote_path` has the finished file's base name (trimmed, `\` read as `/`,
case-insensitive, like Node's `printerFilenamesMatch`). Newest means the
greatest `created_at`, ties broken by link ID, never the position in the
stored list, which a save reorders. Only that newest link is considered: when
it already has a measured length, the finish records nothing, and no older
link is filled in its place. The observer records nothing when:

- it did not see the print running, such as a `complete` already showing at
  startup, or after it lagged behind the transition stream;
- the read fails, no longer reports that file as finished, or reports no
  positive length (the read is not retried);
- no `watching`, `awaiting_verify` or `verified` link names the file;
- the newest matching link already has a measured length, such as when the
  same file name is printed again from the printer's own history: the first
  measurement stays, and an older unmeasured send of that file (for example a
  cancelled one) keeps its estimate.

In each case the estimate stays. The observer never changes Checkoff state and
never dispatches a send, and it does not port Node's guess that a print near
100% that turns idle has finished.

`awaiting_verify` and `verified` links count because the link can move on between the finish
and the measured write. Node owns the host-completion transition: its
`POST /printer-checkoff/reconcile` (`reconcilePrinterCheckoff`, polled by the
old UI) moves a finished `watching` link to `awaiting_verify`, and a fast verify
then makes it `verified`. That route is still unresolved in the Rust gateway's
ownership manifest, and the Rust-only runtime has no reconcile of its own: its
only move out of `watching` is `manual-complete`, which is for `manual:`
printers that no watcher observes. The observer writes only the grams row and
never the link's state.

The Spoolman shortfall read (`GET /plans/:id/filament-shortfalls`) now reads
grams from this table instead of `print_jobs.filament_consumed_g`, which
nothing writes. A send's grams are spread evenly over its `units`, so a send
whose units are only partly checked off still costs each checked-off unit its
share. Each Part averages its five most recent sends that have a checked-off
unit for it (`print_job_parts` rows with the send's `link_id`). A Part with any
measured send uses only measured sends and reads `measured`; a Part with only
estimates reads `estimated`. When one of those estimated sends has a
measured length but no `grams_per_mm`, its Part and filament read
`estimated` with `reason: measured_length_not_convertible`. The read no longer requires
`print_jobs.status = 'completed'`, which nothing sets either.
