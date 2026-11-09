# Security

Print Partner is designed for self-hosting on a trusted network. The default Compose setup is not a hardened public internet service.

## Report a vulnerability

Do not open a public issue for a suspected vulnerability. Use [GitHub private vulnerability reporting](https://github.com/Print-Partner/PrintPartner/security/advisories/new). Include the affected version, deployment mode, reproduction steps, and likely impact. Do not include live credentials or user data.

## Deployment boundary

The default container listens on port 8080 and may be reachable from the local network. Before allowing remote access:

1. place the app behind an HTTPS reverse proxy
2. set `PRINT_PARTNER_API_KEY` for API and MCP access
3. enable Basic authentication or multi-user authentication for browser access
4. restrict inbound traffic at the host firewall
5. set `TRUST_PROXY=1` only when requests pass through a proxy you control

Do not rely on forwarded headers to turn a remote request into trusted loopback traffic.

## Credentials

Store credentials in environment variables, Docker secrets, or the product's secret fields. Never commit them to the repository.

Sensitive values include:

- `PRINT_PARTNER_API_KEY`
- session and Basic authentication secrets
- OAuth client secrets
- SMTP credentials
- GitHub personal access tokens
- printer and Spoolman credentials

API keys created through Print Partner are shown in plaintext once and stored as hashes. Each key has administrator authority. Use separate keys when you need independent revocation.

## MCP

The HTTP MCP endpoint is `/api/v1/mcp`. Every request needs a valid API key, including loopback and LAN clients. Use HTTPS or an authenticated tunnel for a remote MCP client.

Mutating MCP tools create a proposal and require `confirm_apply`. The server does not expose an unconditional print-start tool.

Do not run the stdio MCP server against the live SQLite directory while the web server is running. Use an offline copy or stop the web server first.

## Outbound connections

User-configured integrations can make outbound requests. Print Partner validates destination URLs and redirect targets before connecting.

Cloud metadata addresses remain blocked. Self-host printer and Spoolman adapters may connect to private LAN addresses because that is their intended use.

Review every integration URL before saving it. Keep printer firmware and unauthenticated services on a trusted network.

## Uploads and archives

Zip files, Source archives, backups, and print artifacts are untrusted input. The server applies route-specific upload, field, file-count, and expansion limits. Ordinary JSON requests are limited to 1 MiB, bulk Plan and manifest edits to 8 MiB, and assistant or MCP action bodies to 96 MiB. MCP inline files are limited to 64 MiB. Source uploads are limited to 256 MiB, kit and print-file uploads to 64 MiB, rendered thumbnails to 1 MiB, and backup uploads to 20 GiB. Source archives can expand to at most 1 GiB of stored content. The server also normalizes filenames and resolves extracted files under controlled directories.

These limits reduce resource-exhaustion risk, but they are not storage quotas or reservations. Source and backup uploads stream to disk; kit bundles, print files, thumbnails, and JSON bodies are held in memory up to their limits. Archive processing can temporarily require additional disk space.

Operators should also:

- monitor free disk space under `/data`
- set lower request limits at the reverse proxy when the deployment needs them
- avoid importing archives from unknown sources
- keep antivirus or malware scanning at the host boundary when required by local policy

## Data and backups

Self-host mode stores application state, source files, and exports under `PRINT_PARTNER_DATA_DIR`. SQLite relies on filesystem permissions for confidentiality.

The Docker image prepares `/data`, then runs the Node process as `ppuser` with uid 1000. Do not force `user: "1000:1000"` on an unprepared named volume because the entrypoint would be unable to correct its ownership.

Backups may contain source metadata, Build names, file paths, job history, and other application state. Protect downloaded archives as carefully as the live data directory. Store an encrypted copy outside the Docker volume and test restores.

## Multi-user and SaaS mode

Multi-user mode adds login sessions and tenant-scoped records. It requires a strong `SESSION_SECRET` and should use HTTPS.

The Postgres adapter is experimental because its synchronous compatibility bridge does not provide the same transaction semantics as SQLite. Production startup requires `POSTGRES_EXPERIMENTAL=1`. This flag acknowledges the limitation; it does not harden the deployment.

## Webhooks

Webhook destinations pass outbound URL validation. Delivery follows validated redirects and uses a bounded timeout. Configure a webhook secret and verify the signature before accepting an event.

Do not send webhook secrets in URLs. Remove unused webhooks and rotate a secret if a receiver or log system exposes it.

## Logs and metrics

Application logs can contain route names, resource ids, filenames, integration errors, and operational timing. Export them only to trusted systems. Review logs before attaching them to a public issue.

Protect `/metrics` on remote deployments. Metrics can reveal Build names, printer state, and workload patterns.

## Dependency maintenance

CI runs the quality suite and fails on high-severity npm audit findings. Maintainers should update the lockfile deliberately and rerun:

```bash
cd web
npm ci
npm audit --audit-level=high
npm run quality
```

## Incident response

If you suspect a compromise:

1. disconnect public access or stop the container
2. preserve container logs and an offline copy of `/data`
3. revoke API keys and rotate external integration credentials
4. review user accounts, OAuth clients, webhooks, and printer hosts
5. restore from a known backup if application state changed
6. update the image before returning the service to the network

See [Operations](OPERATIONS.md) for backup, logging, key rotation, and recovery commands.

## Development checklist

Changes that cross a system boundary should include tests for:

- input validation and error responses
- tenant ownership in multi-user mode
- path traversal and archive extraction
- outbound URL and redirect validation
- secret redaction
- upload size and timeout limits
- idempotency or reconciliation for external mutations

Run `npm run quality` from `web` before publishing a change.
