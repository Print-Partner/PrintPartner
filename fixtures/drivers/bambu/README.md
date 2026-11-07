# Bambu LAN MQTT status fixtures

These are test-derived status payloads and one synthetic AMS example for the
LAN MQTT `pushall` status request, not recordings from a live printer. Only the relevant `print`
fields are retained; these are not complete firmware reports.

Sources in `web/apps/server/src/integrations/adapters/bambu.test.ts`:

- `pushall-idle.json`: `testConnection uses MQTT pushall and maps idle report`.
- `pushall-printing.json`: `maps progress, filename, and ETA minutes → seconds`.
- `pushall-error.json`: the `FAILED` token in `maps known tokens`, wrapped
  in the same `print` report envelope as the MQTT tests.

`ams-tray-synthetic.json` is a **synthetic, unverified** example created for
GRE-559 because the existing tests have no AMS tray payload. Its idle status
comes from the idle test above; the nested AMS unit, tray, material, and color
are invented test data, not a live capture or an existing test excerpt.
Replace or verify the AMS fields against a sanitized live capture before
using them as protocol evidence.

H2D-specific fields are **unverified until a live capture**. These fixtures
do not establish H2D dual-nozzle, multi-AMS, or firmware-specific compatibility.
The TS status adapter currently reads print status and temperatures, not AMS
tray metadata. Do not add device serials, access codes, or IP addresses to
these public fixtures.

From `web`, replay every JSON fixture through the existing TS adapter:

```sh
npm run test -w @print-partner/server -- src/integrations/adapters/bambu.fixtures.test.ts --reporter=verbose
```

The Vitest suite name includes the discovered fixture count. Every JSON file
must have an expected adapter status in the test.

`driver/prints.json` lists these four fixtures by path plus inline `print`
objects for edge cases (string and hex numbers, padded tokens, blank and
missing fields). `driver/node-oracle-status.json` is the Node adapter's status
for each case, and the Rust driver test
(`rust/crates/pp-drivers/tests/ticket_t_23_bambu.rs`) checks its own mapping
against it. Regenerate it from `web` after changing a case:

```sh
UPDATE_BAMBU_ORACLE=1 npm run test -w @print-partner/server -- src/integrations/adapters/bambu.driver-oracle.test.ts
```
