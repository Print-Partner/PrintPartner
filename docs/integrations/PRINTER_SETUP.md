# Connect a printer

Print Partner separates printer geometry from network connections:

- A **Printer** records bed size, name, and planning settings.
- A **printer host** records the Moonraker, PrusaLink, or Bambu connection and its secret.

Create both records, then link the Printer to the host.

## Moonraker

1. Open **Settings**, then **Printers**.
2. Add a printer host and choose **Moonraker**.
3. Enter a name and base URL such as `http://192.168.1.40:7125`.
4. Add an API key or JWT if the Moonraker server requires one.
5. Save the host and select **Test connection**.

Print Partner uses Moonraker for status, G-code upload, and optional print start. It sends `X-Api-Key` for API keys and bearer authentication for JWTs.

## PrusaLink

1. Open **Settings**, then **Printers**.
2. Add a printer host and choose **PrusaLink**.
3. Enter the printer's base URL.
4. Enter the PrusaLink username and password or API credentials requested by the form.
5. Save and test the connection.

Print Partner uses PrusaLink digest authentication for status and file operations. Supported formats depend on the printer and PrusaLink version.

## Bambu LAN status

1. Enable LAN mode on the printer.
2. Record its access code, serial number, and LAN address.
3. Open **Settings**, then **Printers**.
4. Add a printer host and choose **Bambu LAN**.
5. Enter the LAN values, save, and test the connection.

The LAN connection provides status only. Print Partner does not send reverse-engineered MQTT print-start commands.

For file handoff on Rust desktop, choose a sliced file on the Build's Production
tab and press **Stage file**. After staging, press **Open in Bambu Connect**.
Bambu Connect remains responsible for importing and sending the file.

The Rust desktop route stages one local `.3mf`, `.gcode.3mf`, `.gcode`, or
`.gco` file (up to 64 MiB) for a Build with Bambu enabled. It returns an
**Open in Bambu Connect** link, not a send receipt. The operator opens the
link, then chooses a printer and presses **Send** in Bambu Connect. Staging
does not change Checkoff or contact a printer. A repeat of the same Build,
file type and bytes reuses the staged file. A later handoff removes Rust's
staged files unused for 24 hours and refuses a new file if 16 are still
retained. The Rust route does not stage for Docker or LAN clients. The
replacement Production screen does not expose this route yet.

The Rust launcher proof (GRE-2018) stages a synthetic file through
`POST /api/v2/plans/{id}/bambu-connect-handoff`. It checks that the returned
link names a private staged file eligible for the native command, that
staging the same bytes after restart reuses the file, and that a conflict
leaves the existing bytes intact. Headless Rust does not mount that POST.
Neither Rust runtime serves the legacy stage and download routes at
`/bambu-connect/handoff` and `/bambu-connect/handoff/{id}/file`, including
their `/api/v1` aliases. The proof opens neither Connect nor a printer and
finds no job or published export. Legacy production ownership stays on Node.

## Create and link a Printer

1. Open **Printers** from the main navigation.
2. Add a Printer from a preset or enter custom bed dimensions.
3. Select the printer host you created.
4. Save the Printer.

The bed dimensions drive plate packing. The linked host provides status and send capabilities. A Printer can still be used for planning without a live host.

## Send a sliced file

Print Partner does not slice models. Export a 3MF or STL pack, slice it in your chosen slicer, then return the resulting `.gcode` or `.bgcode` file to Production.

1. Open the Build's **Production** page.
2. Select **Send to printer**.
3. Choose a linked Moonraker or PrusaLink Printer.
4. Choose the sliced file.
5. Select **Upload**, **Upload and start**, or **Queue for idle**.

Starting is blocked when the selected printer reports a busy state. Queued jobs stay pinned to the selected Printer.

## Confirm completed work

When a sent job finishes, Print Partner adds a verification item to Checkoff. Review the host result and confirm or reject it. A successful printer status never marks a unit complete without this review.

Rejected items can record a reason for later reporting.

## Status meanings

| Status | Meaning |
|--------|---------|
| Idle | The host is reachable and has no active job. |
| Printing | The host reports an active job. Progress and ETA appear when available. |
| Complete | The host reports a successful finish that may need Checkoff review. |
| Busy | The host cannot start another job. |
| Offline | Print Partner could not reach the host. |
| Error | The adapter or remote host returned an error. |

## Network safety

Printer hosts usually use private LAN addresses. Keep Print Partner and printer firmware on a trusted network. Use HTTPS when credentials cross an untrusted network.

Print Partner validates outbound URLs and blocks cloud metadata addresses. Self-host mode permits private LAN destinations because local printer connections require them.

## Troubleshooting

### Connection refused

Confirm the address and port from the Print Partner host, not only from your laptop. A container must be able to route to the printer's LAN address.

### Authentication failed

Re-enter the host secret and test again. Do not include a token in the base URL. Check the printer or host clock if digest or certificate validation behaves unexpectedly.

### File upload failed

Confirm that the file is already sliced and uses a format accepted by the target printer. Rename the file if PrusaLink reports that it already exists.

### Upload succeeded but start failed

Check the printer's current job and file list before retrying. The file may already exist on the host even when the start request timed out.

### Printer remains offline

Check firewall rules, VLAN routing, mDNS assumptions, and whether the printer entered sleep mode. Use an IP address while diagnosing name resolution.

## Related documentation

- [Spoolman](SPOOLMAN.md)
- [Architecture](../architecture.md)
- [HTTP API](../api.md)
- [Operations](../../OPERATIONS.md)
