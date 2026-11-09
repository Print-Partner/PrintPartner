# Operations

This guide covers the supported self-host Docker deployment. See [Install](docs/INSTALL.md) for first-time setup and [Deployment reference](web/DEPLOY.md) for all environment variables.

The private invite host uses `docker-compose.hosted.yml`. HTTP `/backups` is off there. Back up the `print-partner-hosted-data` volume instead. See [Invite-beta hosted planning](web/DEPLOY.md#invite-beta-hosted-planning).

## Check the service

```bash
docker compose ps
docker compose logs --tail 100 print-partner
curl --fail --silent http://127.0.0.1:8080/health | jq .
```

<!-- release-version:start -->
A healthy `3.3.0` deployment reports runtime version `3.3.0-web` and a release tag of `v3.3.0`.
<!-- release-version:end -->

The health response also reports the database driver and connection state. Treat a failed database check as unavailable service, even when the container remains running.

## Authenticate administrative requests

Local loopback requests may use the trusted self-host shortcut. Remote and reverse-proxied requests should send an API key:

```bash
export PP_API_KEY='replace-with-your-key'
curl --fail \
  -H "Authorization: Bearer $PP_API_KEY" \
  https://print-partner.example/api/v1
```

All current API keys have administrator authority. Store them as secrets and issue separate keys when independent revocation matters.

The curl example above uses the open self-host deployment key, `PRINT_PARTNER_API_KEY`. With `MULTI_USER=1`, send the login session as well. A bearer key on its own is rejected. The administrator creates Settings keys for the signed-in tenant. That stored key is separate from `PRINT_PARTNER_API_KEY`.

## Back up data

Create a server-side backup:

```bash
curl --fail --silent -X POST http://127.0.0.1:8080/backups | jq .
```

List stored backups:

```bash
curl --fail --silent http://127.0.0.1:8080/backups | jq .
```

Inspect current storage use, estimated backup contents before compression, and
free space on the data filesystem:

```bash
curl --fail --silent http://127.0.0.1:8080/backups/storage | jq .
```

The Settings backup panel presents the same inventory by Database, Source
revisions, Working Source files, Exports, Thumbnails, Covers, Assistant
knowledge, Configuration, Stored backups, and Other application data. The
estimate is a point-in-time sum of logical file sizes. It is not the exact
compressed archive size or allocated block count.

A format v2 full backup covers 11 durable roots: `repos`, `sources`, `exports`,
`thumbs`, `covers`, `assistant-domain`, `manifests`, `custom_filaments.json`,
`kit-catalog.json`, `kit-catalog.yaml`, and `path-hints.yaml`. You can add
deployment-specific path conventions to `/data/path-hints.yaml`. Print Partner
validates that file and merges its rules with the packaged generic conventions.
If a full v2 archive declares a covered root but contains no entry for it,
restore removes the current root. A v2 database-only archive leaves all 11
roots unchanged. Legacy v1 archives replace only legacy roots that are present
in the archive, so an omitted path is retained.

The built-in backup format allows at most 20 GiB of expanded archive content,
8 GiB for one archive entry, and 100,000 entries. The server validates a new
backup against these restore limits before it publishes the archive. If the
estimated contents approach 20 GiB, use a tested filesystem or volume snapshot
instead. Do not delete immutable Source revisions or accepted exports without
a retention policy that explicitly permits the deletion.

Download one archive:

```bash
export PP_BACKUP_NAME='print-partner-backup-2026-08-22T12-00-00-000Z.tar.gz'
curl --fail \
  "http://127.0.0.1:8080/backups/$PP_BACKUP_NAME" \
  --output "$PP_BACKUP_NAME"
```

The server stores its backup archives under `/data/backups`. Keep another copy outside the Docker volume. A backup stored only beside the live database does not protect against volume loss.

Back up before upgrades, major imports, or storage changes.

## Restore a backup

List backups and choose the exact stored filename. Check the archive and
available capacity before restoring:

```bash
export PP_BACKUP_NAME='print-partner-backup-2026-08-22T12-00-00-000Z.tar.gz'
curl --fail --silent \
  "http://127.0.0.1:8080/backups/$PP_BACKUP_NAME/preflight" | jq .
```

The preflight reports the validated, uncompressed archive bytes, required free
space including a safety reserve, current free space, and whether capacity is
sufficient. Free space can change after this check. The server repeats the
check immediately before extraction and returns HTTP 507 before closing SQLite
when capacity is insufficient.

The compressed upload limit is 20 GiB. Independently, validation rejects more
than 20 GiB of expanded content, any entry larger than 8 GiB, or more than
100,000 entries.

Restoring replaces current application data. Stop or finish Source syncs,
imports, printer transfers, and other mutation jobs first. One server process
rejects overlapping restore requests, but it does not drain background jobs or
coordinate restore state with another Print Partner process that shares the same
data directory.

```bash
export PP_BACKUP_NAME='print-partner-backup-2026-08-22T12-00-00-000Z.tar.gz'
curl --fail -X POST http://127.0.0.1:8080/backups/restore \
  -H 'Content-Type: application/json' \
  --data "{\"backupName\":\"$PP_BACKUP_NAME\"}"
```

Restart the service after restore, then check health and application state:

```bash
docker compose restart print-partner
curl --fail --silent http://127.0.0.1:8080/health | jq .
```

Test restore procedures on a disposable copy before depending on them for recovery.

Restore validates and extracts into a sibling staging directory before it
closes SQLite. It activates each top-level directory and the database with
same-filesystem renames, removes stale SQLite journal files, and rolls the
activated paths back if activation or reconnect fails. If rollback itself
fails, the server preserves the recovery workspace and reports the recovery
errors. A process or host crash during activation still requires manual
inspection because restore has no durable crash journal.

## Verify a release

Each release signs its image by digest and its `release-identity.json` asset with the Print Partner release key. The public key is [`cosign.pub`](cosign.pub) in this repository, and each GitHub Release carries a copy. Install [cosign](https://docs.sigstore.dev/cosign/system_config/installation/), then download `release-identity.json`, `release-identity.json.sigstore.json` and `cosign.pub` from the release:

```bash
TAG=v1.2.3
gh release download "$TAG" -R Print-Partner/PrintPartner \
  --pattern release-identity.json --pattern release-identity.json.sigstore.json --pattern cosign.pub
```

Confirm the downloaded `cosign.pub` matches the one in the repository at that tag, then verify the identity asset:

```bash
cosign verify-blob --key cosign.pub \
  --bundle release-identity.json.sigstore.json release-identity.json
```

The asset records the image digest. Verify the image by that digest, not by a tag:

```bash
DIGEST="$(jq -r .image.digest release-identity.json)"
cosign verify --key cosign.pub "ghcr.io/poitee/print-partner@$DIGEST"
```

Both commands print `Verified OK` or a list of verified signatures and exit 0. A non-zero exit means the artifact was not signed by the release key. Do not run it.

## Update

Read [CHANGELOG.md](CHANGELOG.md), then update:

This release upgrades databases from Print Partner v3.3.0 (schema 31) or newer. An installation on an older release must step through v3.3.0 first: install v3.3.0, start it once so it migrates the database, confirm it opens an existing Build, then install this release. Startup refuses an older SQLite or Postgres database before it creates a backup or changes any data, and the error names v3.3.0.

Run only one Print Partner version against a self-host data directory during an upgrade. Do not use a rolling deployment that shares the SQLite database or `/data` between old and new application instances. The startup migration preserves a concurrently changed legacy Source manifest as a recoverable backup, but it cannot make an uncoordinated old writer part of the new revision transaction.

If the installation first enabled `MULTI_USER` on v3.3.0 after it already held
data, retain an offline pre-upgrade backup. That release could assign only part
of the bootstrap tenant to the first account. The current upgrade prevents that
partial claim for new accounts. It repairs an active Source ownership mismatch
only when the result is unambiguous. It does not generally recover or re-tenant
inactive Source history, and it stops startup for manual recovery when a
protected revision conflicts. Review ownership on every installation that ran
the v3.3 claim path. Do not discard the backup until inactive Source revisions,
Source documents and notes, Plan drafts and revisions and their dependent
history, accepted Plates, slicer and printer configuration, print jobs,
telemetry, and events have been checked under the first account. Split or
conflicting history requires graph-aware recovery.

```bash
git pull --ff-only
docker compose pull
docker compose up -d
```

Before Print Partner opens an existing SQLite database with a new application release, it creates and validates a database backup under `/data/backups`. Startup stops before migration if the backup fails. The backup filename identifies the target application and schema versions. Restarting the same release reuses the backup instead of creating duplicates.

This automatic backup protects database records during an update. Continue to
create and download a full backup before storage changes or major imports. The
full-backup scope is listed in [Back up data](#back-up-data).

Watch startup and migration logs:

```bash
docker compose logs -f print-partner
```

Confirm the reported version and open an existing Build before considering the upgrade complete.

Print Partner refuses to open a database whose schema is newer than the application supports. To roll back across a schema change, restore the matching pre-update backup instead of starting an older image against the migrated database.

If you build locally:

```bash
git pull --ff-only
docker compose build --pull
docker compose up -d
```

## Roll back

A database migration may make old application code incompatible with new data. Do not roll back only the image.

1. stop the service
2. select the previous image version
3. restore a backup created by that version
4. start the service
5. verify health and a representative Build

Keep versioned images and matching backups until the upgrade has been proven.

## Logs

Container logs:

```bash
docker compose logs --tail 200 print-partner
docker compose logs -f print-partner
```

Application workflow logs:

```bash
curl --fail --silent http://127.0.0.1:8080/settings/logging/stats | jq .
curl --fail --silent 'http://127.0.0.1:8080/settings/logging/logs?limit=100' | jq .
curl --fail --silent \
  'http://127.0.0.1:8080/settings/logging/export?format=jsonl' \
  --output print-partner-logs.jsonl
```

In Settings → Data & System, **Workflow Tracking** controls capture of new HTTP
requests and integration events. Turning it off preserves existing logs and does
not disable general server diagnostics. The toggle and severity setting are
in-memory settings; they reset when the server restarts. API clients can update
`enableWorkflowTracking` and `minSeverity` through `POST /settings/logging/config`.
`maxLogs` is read-only.

Clear in-memory workflow logs only after exporting anything you need:

```bash
curl --fail -X DELETE http://127.0.0.1:8080/settings/logging/logs
```

Review exported logs for filenames, resource ids, and other sensitive context before sharing them.

## API keys

Create a key:

```bash
curl --fail --silent -X POST http://127.0.0.1:8080/settings/api-keys | jq .
```

The plaintext key appears once. Store it before closing the response.

List keys:

```bash
curl --fail --silent http://127.0.0.1:8080/settings/api-keys | jq .
```

Rotate or revoke a key by id:

```bash
export PP_KEY_ID='key-id'
curl --fail --silent -X POST \
  "http://127.0.0.1:8080/settings/api-keys/$PP_KEY_ID/regenerate" | jq .
curl --fail -X DELETE \
  "http://127.0.0.1:8080/settings/api-keys/$PP_KEY_ID"
```

## Metrics

Prometheus metrics are available at `/metrics`:

```bash
curl --fail http://127.0.0.1:8080/metrics
```

Remote deployments may require a session or API key. Protect metrics at the same network boundary as the application.

Monitor at least:

- health check failures
- HTTP error rate
- remaining disk space under `/data`
- backup age
- printer integration failures
- Build and printer workload gauges relevant to your shop

## Disk and volume checks

```bash
docker compose exec print-partner du -sh /data
```

Compose prefixes named volumes with the project name, so `docker volume inspect print-partner-data` often misses the real volume.

The Settings backup panel shows the server-side breakdown and estimated backup
contents. Large consumers are usually immutable Source revisions, Working
Source files, exports, thumbnails, and backup archives. Print Partner does not
automatically delete Source revisions, exports, or stored backups. Download old
backups before deleting their server-side copies, and do not remove revision or
export files directly from `/data`.

The default image prepares `/data` as root, then runs Node as uid 1000. Do not add a fixed `user:` setting unless a bind mount has already been prepared for that uid.

Verify the app process:

```bash
docker top "$(docker compose ps -q print-partner)" -eo uid,user,pid,cmd
```

## SQLite integrity

Create a backup before manual database work. The production image may not include the SQLite CLI, so run integrity checks from a maintenance container or a host tool against an offline copy.

Never point two application processes at the same SQLite database. This includes the stdio MCP server and ad hoc migration scripts.

## Common failures

### Container does not start

Check the Compose status and logs. Common causes are a port conflict, invalid environment value, failed database migration, or `/data` permissions.

### Permission denied under `/data`

Remove a `user:` override from the default named-volume setup. For a bind mount, stop the container and set the directory owner to uid 1000 before restarting.

### Image pull is denied

Build from source with `docker compose up --build -d` while registry access is unavailable.

### Printer or Spoolman is unreachable

Test the destination from the Docker host. Confirm VLAN routing, firewall rules, DNS, and the address visible from inside the container. See [Printer setup](docs/integrations/PRINTER_SETUP.md) and [Spoolman](docs/integrations/SPOOLMAN.md).

### A browser path returns JSON

Check the reverse proxy. Browser document navigation must preserve `Sec-Fetch-Mode: navigate` or an `Accept` header containing `text/html`.

## Incident response

For suspected compromise:

1. remove public access or stop the container
2. preserve container logs and an offline copy of `/data`
3. revoke API keys and rotate integration credentials
4. inspect user accounts, webhooks, OAuth settings, and printer hosts
5. restore from a known backup if state changed
6. update and verify before reconnecting the service

See [Security](SECURITY.md) for the reporting process and hardening guidance.
