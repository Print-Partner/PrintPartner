# Settings Integrations

The Integrations section of replacement Settings (GRE-1338) lists the saved
integrations from `GET /api/v2/integrations` through the generated
`listIntegrations` client. Each row shows the name, kind, on or off, the
address when the core has one, the username when one is set, and whether an API
key or password is saved. The list route answers only `has_api_key` and
`has_password`, never the key or password, so the section cannot show a secret.
Its tests feed a list that carries a secret anyway and check it never reaches
the page. The address is shown without any user name, password, query or
fragment, with "Sign-in details stored in this address are hidden." when it
had any; text that is not a URL shows "The stored address can't be shown."
The core already leaves those parts out of the list (GRE-1342), and an address
with sign-in details is never connected to; the screen does it again as a
second line of defence. Adding and editing integrations are separate changes. Adding or
deleting a printer in the Printers section also writes its connection, so it
reads this list again.

The Spoolman row says that Spoolman alone owns filament counts and that stock
it cannot read shows as unknown, never as zero. Settings has no stock read; the
unknown stock itself shows where stock appears, on Builds.

## Bambu certificate pin

Each Bambu row reads its pin from `GET /api/v1/integrations/{id}/tls-pin`
(`readTlsPin`) and shows either "Certificate pinned · <12-hex prefix> · <date>"
or "Not pinned. Test connection to pin." Only a Bambu Test connection records
a pin (GRE-1245, [Bambu LAN](../bambu-lan.md)). A pin that cannot be read shows
"Certificate state unavailable" with the core's reason.

A pinned row has **Reset certificate**. It opens the shared `Dialog`, which
warns that the next Test connection trusts whatever certificate answers, so run
it only while the printer on the network is this same device. Focus starts on
Cancel. Only the dialog's own Reset certificate button sends
`POST /api/v1/integrations/{id}/tls-pin/reset` (`resetTlsPin`), once, from the
signed-in browser. The reset belongs to the row, not the dialog, so closing the
dialog while it is in flight loses nothing:

- Confirmed: the row shows "Not pinned" at once, even if the next read fails.
- Refused (a 4xx such as the route's 5-per-minute limit): "The certificate was
  not reset" with the core's reason. The pin is unchanged; Reset works again.
- Unconfirmed (no answer, an unreadable one, or a 5xx): the core may have
  forgotten the pin. The row says the core didn't confirm the reset and reads
  the pin again; Reset stays off until that read answers.

Every reset reads the pin again once it settles. A pin read that fails shows
"Certificate state unavailable", never the old pin, with **Check the
certificate again**. The pin read has its own 60-per-minute window; the
section reads one pin per Bambu row.

## States

Loading, empty ("No integrations yet."), a failed read with the core's reason
and **Reload integrations**, a failed refresh that keeps the last list, and
offline. Offline keeps the last list, says why changes are off, and Reset
certificate stays focusable but opens nothing. The whole Settings screen keeps
its own states from [Settings Modules](settings-modules.md).

Verify with `npx vitest run src/settings` in `web/apps/ui`, the ui package's
lint and typecheck, and a `/settings` screenshot on a Rust-only core with
disposable data.
