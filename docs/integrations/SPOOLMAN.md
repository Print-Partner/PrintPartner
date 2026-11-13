# Connect Spoolman

Print Partner can read filament and spool inventory from a Spoolman server. Builds can use that inventory for role colors and per-part spool assignments. Confirmed printer jobs can deduct estimated material when the host reports filament usage.

## Requirements

- A running Spoolman server reachable from the Print Partner container
- The Spoolman base URL, usually `http://<host>:7912`
- An API key if your Spoolman deployment requires one

## Add the integration

1. Open **Settings**.
2. Find **Optional integrations** under the Library settings.
3. Select **Add Spoolman**.
4. Enter a name, base URL, and optional API key.
5. Save and test the connection.

Use the URL that the Print Partner container can reach. `localhost` inside the container refers to the container itself, not the Docker host.

## Docker networking

If Spoolman runs in the same Compose project, address it by service name:

```text
http://spoolman:7912
```

If it runs on another LAN computer, use that computer's address:

```text
http://192.168.1.50:7912
```

Docker Desktop also provides `host.docker.internal` for services running directly on the host:

```text
http://host.docker.internal:7912
```

On Linux, a LAN address or shared Docker network is usually clearer than host gateway aliases.

## Choose filament for a Build

Open a Build and go to **Sources**. Assign a filament color or Spoolman filament to each role. You may also choose a specific spool when the Build must use inventory from one location.

The Plan and Checkoff screens show the available weight reported by Spoolman. Print Partner does not treat this value as a reservation.

Per-part overrides can select a different spool from the role default. Clearing an override returns the part to the role setting.

When Plan feature controls are available, disabling Spoolman for a Plan hides its inventory choices and physical spool selectors. Restoring access reloads the inventory. If a role assignment is denied, the picker refreshes access and shows the save error.

Importing a color preset while Spoolman is disabled for the Plan removes its Spoolman filament and spool references. Catalog colors and custom hex colors still import. If removing Spoolman references leaves a role without a color, the import skips that role and keeps its stored assignment. If access changes during an import, it may stop after saving some roles; the picker reloads those assignments.

## Filament shortfalls

The Rust core compares the grams a Build still needs with the grams Spoolman reports. It needs the Spoolman Module turned on; while it is off, the check reports the off reason and makes no Spoolman request.

- **Grams needed come from past prints.** Each send records the slicer's estimate, and a finished print replaces it with the grams the printer measured (GRE-1165). Print Partner divides a send's grams by the units it printed and averages the five most recent sends with a checked-off unit of that Part. Measured sends win: a Part uses its estimated sends only when none is measured. Grams needed shows its basis: `measured`, or `estimated` while any Part relies on an estimate. A Part with no checked-off unit under a send with grams has unknown need (`grams_not_recorded` when no Part of the Build has grams, `part_not_printed` otherwise), and no shortfall is reported for its filament.
- **Grams available come from Spoolman**, summed over the filament's unarchived spools. If Spoolman cannot be reached, does not know the filament, or a spool has no remaining weight, stock is unknown, not zero. The check reads plain `http://` Spoolman URLs only; an `https://` URL reads as unknown.
- A shortfall is reported only when both numbers are known. Reading the check never reserves or deducts stock.

The Build Overview's **Filament** section shows the check (GRE-1102). It reads when the Overview opens and when the owner presses Refresh, and says when it read. Each row names the filament (by its filament ID when no label is known), the grams needed with their basis, the grams in Spoolman with the number of spools, and Enough, Short by N g, or Not known, with its Parts and their remaining units. Unknown stock reads "Not known" with the reason, never 0 g. With the Module off it shows the Module-off notice and makes no request to Spoolman.

## Material deductions

Print Partner can deduct material after a linked printer job reaches Checkoff and the user confirms the completed units.

The deduction requires:

- a Spoolman spool assignment for the affected part
- a printer adapter that reports filament usage
- a completed host job linked to the Checkoff units
- user confirmation in Checkoff

Moonraker reports `print_stats.filament_used`. PrusaLink reports its consumed material value when available. Print Partner converts the reported length to weight using filament properties and sends the usage to Spoolman.

If Spoolman is unavailable or the required mapping is missing, Checkoff still completes and reports the deduction problem separately.

### On the Rust core

The Rust core records each deduction when you confirm a Checkoff burst, in the same transaction, and does not call Spoolman then. It records one `pending` row per pinned spool, with the grams to deduct: the link's grams (the slicer estimate, or the measured grams once the print finished) divided by the link's units, times the confirmed units whose Part pins that spool (`spoolman:{integration}:spool:{n}`). Units it cannot deduct give one `skipped` row with the reason: `no_grams`, `no_pinned_spool`, `integration_missing`, `integration_disabled`, `integrations_unreadable` or `module_setting_unreadable` (a stored Module setting is not the typed shape, so the burst still succeeds). With the Spoolman Module off for the Build, nothing is recorded. Repeating a burst records nothing new, and undoing it removes its `pending` and `skipped` rows. `GET /api/v2/spoolman/deductions` lists the rows.

Deduction runs whenever the Spoolman Module is on; there is no per-printer setting. Do not also set `[spoolman]` in a printer's `moonraker.conf`, or the printer's own usage reports and these deductions would both count.

Sending each `pending` row to Spoolman once, and reconciling an uncertain result, follow in later parts of GRE-411.

## Security

Store the Spoolman API key in the integration secret field. Print Partner redacts integration secrets from normal API responses and logs.

Keep unauthenticated Spoolman servers on a trusted network. Use HTTPS when traffic crosses an untrusted network.

## Troubleshooting

### Connection refused

Test the URL from the Print Partner host or container. Confirm that a firewall or Docker network is not blocking port 7912.

### Filaments do not appear

Confirm the integration test succeeds, then reload Sources. Check that Spoolman contains filament records and that the selected integration remains enabled.

### Weight is missing

A filament may exist without an active spool. Select a specific spool or add inventory in Spoolman.

### Usage was not deducted

Confirm the part has a specific Spoolman spool, the printer job is linked to the Checkoff units, and the printer adapter reported material use. Review server logs for a failed Spoolman request.

## Related documentation

- [Printer setup](PRINTER_SETUP.md)
- [Architecture](../architecture.md)
- [Operations](../../OPERATIONS.md)
