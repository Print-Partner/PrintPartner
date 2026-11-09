# Deploying Print Partner Web

**First time with Docker?** See the beginner install guide: [docs/INSTALL.md](../docs/INSTALL.md).

## Self-host (default)

### Docker Compose

From the repository root:

```bash
docker compose pull && docker compose up -d
```

Open [http://localhost:8080](http://localhost:8080), or `http://<this-computer>:8080` from another machine on the LAN. Compose publishes `8080` on every host interface. Set `PP_BIND_ADDRESS=127.0.0.1` to keep it on this computer, and enable authentication on an untrusted network. Data persists in the `print-partner-data` volume (`/data` in the container). To build from source instead of pulling, use `docker compose up --build`.

### Published images

<!-- release-version:start -->
Release images are published to GitHub Container Registry:

| Image | Tags | Platforms |
|-------|------|-----------|
| `ghcr.io/poitee/print-partner` | `latest`, `X.Y.Z` (one per release, e.g. `3.3.0`) | `linux/amd64`, `linux/arm64` |

Each image bakes the release version, peeled Git commit, tag, and build date into
its runtime identity. `GET /health` reports those values, and the in-app update
checker compares the runtime version with GitHub Releases. Compose defaults to
the prepared `3.3.0` image tag. Set `PRINT_PARTNER_VERSION` to another release
explicitly. The compose files keep a `build:` section as a fallback, so
`docker compose up --build` always works without the registry.

**Pull failures:** Package visibility is managed once in GitHub Packages, not
during a release. If `docker compose pull` returns `denied` or `unauthorized`,
use `docker compose up --build` instead, or `docker login ghcr.io` with a token
that has `read:packages`. See
[docs/INSTALL.md](../docs/INSTALL.md#the-image-pull-is-denied).

The app service has a healthcheck that polls `GET /health` every 30s using Node's built-in `fetch` (the `node:22-bookworm-slim` runtime image ships no curl/wget). `docker ps` shows the container as `healthy` once the server responds.

### Environment variables (self-host)

| Variable | Default | Description |
|----------|---------|-------------|
| `PRINT_PARTNER_DATA_DIR` | `./data` | SQLite DB, repos, exports, thumbs |
| `HOST` | `127.0.0.1` | Bind address |
| `PP_BIND_ADDRESS` | `0.0.0.0` | Compose published-port interface. Set `127.0.0.1` to keep 8080 on this computer. |
| `PORT` | `18765` (dev) / `8080` (Docker) | HTTP port |
| `STATIC_DIR` | unset | When set, serve built SPA from this directory |
| `DEPLOY_MODE` | `self-host` | `self-host` or `saas` |
| `CORS_ORIGIN` / `ALLOWED_ORIGINS` | `true` | CORS allowed origin(s). Use commas for multiple origins. |
| `PP_VERSION` | `3.3.0-web` (baked into release images) | Health payload version |
| `PP_COMMIT` / `PP_TAG` / `PP_BUILD_DATE` | baked into release images | Read-only release provenance reported by `GET /health`. Source builds report a development identity. |
| `BASIC_AUTH_USER` / `BASIC_AUTH_PASS` | unset | Optional HTTP Basic protection |
| `MULTI_USER` | `0` | Set to `1` to enable multiple tenant-scoped accounts. Requires `SESSION_SECRET`. Host filesystem profile sync is disabled in this mode. |
| `SINGLE_USER_AUTH` | `0` | Set to `1` to require login with one administrator account. The first registration claims existing self-host data. |
| `SESSION_SECRET` | unset | Not needed for single-user auth. Required when `MULTI_USER=1`. |
| `SESSION_COOKIE_SECURE` | `0` | Set to `1` when the self-hosted site is served through HTTPS. Leave at `0` for a plain HTTP LAN address. |
| `SOURCE_DOCS_MAX_BYTES` | `1073741824` | Per-source budget for synced markdown/PDF docs (~1 GiB). Operator escape hatch only. |
| `PRINT_PARTNER_API_KEY` | unset | Open self-host deployment secret. When set, requires Bearer or `X-Print-Partner-Api-Key` on `/api/v1/*`. `/api/v1/mcp` needs this key or a Settings key on every host, loopback included. `MULTI_USER=1` also requires a login session. |
| `OPENAPI_UI` | unset | Set to `1` to expose `/api/v1/docs` in production |
| `REDIS_URL` | unset | Optional. When set in SaaS, enables the BullMQ job queue. See SaaS below. |
| `PRINT_PARTNER_UPDATE_CHECK` | enabled | Set to `0` to disable in-app update checks |
| `GITHUB_REPO` | `Print-Partner/PrintPartner` | GitHub repo for release lookup |
| `PRINT_PARTNER_LATEST_VERSION` | unset | Air-gapped override that skips GitHub and compares against this version |
| `PRINT_PARTNER_UPDATE_CHECK_CACHE_HOURS` | `12` | How long to cache the latest release lookup |
| `ASSISTANT_ALLOW_URL_INGEST` | enabled | Set to `0` to disable MCP `ingest_guide_url` / `fetch_web_page` |
| `ASSISTANT_GUIDE_INGEST_MAX_BYTES` | `524288` (512 KiB) | Max response body size for a single guide / page URL fetch |
| `PRINT_PARTNER_MCP_PLAN_ID` | unset | Optional default `plan_id` for MCP tools that omit it |
| `SLICER_ORCA_DIR` / `SLICER_PRUSA_DIR` / `SLICER_BAMBU_DIR` | `/slicer-profiles/<slicer>` | Profile directory for the corresponding stock slicer instance. Used only in single-tenant self-host mode. |
<!-- release-version:end -->

### Browser error reporting (optional)

The React app initializes `@sentry/react` for errors and tracing only when
`VITE_SENTRY_DSN` has a non-empty value at **build** time. Vite inlines it, so a runtime environment variable on
the container does not reach the browser. Pass a replacement as a Docker build
arg (`docker build --build-arg VITE_SENTRY_DSN=...`) or put it in
`web/apps/web/.env` for local `npm run dev`. `release` is the web package
version and `environment` is the Vite mode. Production source maps are not
uploaded until a Sentry organization, project, and auth token are available, so
minified stack traces stay unreadable until that upload is configured.

Filesystem profile sync is available only in single-tenant self-host mode. It
is disabled in SaaS and when `MULTI_USER=1` because a process-wide mounted
directory cannot establish tenant ownership. Slicer instance configuration and
Docker lifecycle routes require administrator access.

Direct local-folder Sources follow the same trust boundary. Single-tenant
self-host deployments may submit `local_path`. SaaS and multi-user deployments
reject client-supplied paths and store synchronized or uploaded content only
under that Source's managed `repos/<source-id>` workspace. Their Source API
responses set `local_path` to `null`. Use `content_available` to determine
whether server-managed content can be browsed.

**URL ingest safety (MCP tools):** `ingest_guide_url`, `fetch_web_page`, and `web_search` use the same SSRF guard as cover/image fetches (`safeOutboundFetch`): HTTP(S) only, DNS-resolved, private/loopback/metadata blocked. Guide and search text is untrusted evidence. Mutations require confirm-to-apply. There is no autonomous crawler.

### HTTP MCP (preferred on live host)

Streamable HTTP MCP is served by the running app at **`/api/v1/mcp`** (same product tools as stdio). Pending proposes are **per MCP session** (`mcp-session-id`).

**Fail-closed:** `/api/v1/mcp` needs **`PRINT_PARTNER_API_KEY`** or a Settings key on every bind, loopback included. A request with no key returns **503** when no deployment key is set, and **401** when one is. Auth header: Bearer or `X-Print-Partner-Api-Key`.

`MULTI_USER=1` still requires a login session for `/api/v1/mcp`. The deployment key does not select a tenant. `SINGLE_USER_AUTH=1` accepts the deployment key or a Settings API key on that MCP route without a session.

Use **HTTPS** for remote clients (terminate TLS at a reverse proxy). Plain `http://` is only for loopback or an authenticated tunnel.

```json
{
  "mcpServers": {
    "print-partner": {
      "url": "https://print-partner.example/api/v1/mcp",
      "headers": {
        "Authorization": "Bearer YOUR_API_KEY"
      }
    }
  }
}
```

Cursor plugin: [`cursor-plugin/print-partner`](../cursor-plugin/print-partner). Full connect guide: [`docs/assistant-mcp.md`](../docs/assistant-mcp.md).

### Stdio MCP (DATA_DIR copy only)

Stdio opens SQLite directly. **Do not** point it at the live Docker volume while the app is running (two writers). Use a copy of `PRINT_PARTNER_DATA_DIR`, or stop the app.

```bash
cd web
export PRINT_PARTNER_DATA_DIR=./data-copy
export PRINT_PARTNER_MCP_PLAN_ID=1
npm run mcp -w @print-partner/server
```

Requires `DEPLOY_MODE=self-host` (default).

### Checking for app updates


When update checks are enabled (default), the server compares `PP_VERSION` to the latest [GitHub release](https://github.com/Print-Partner/PrintPartner/releases) (cached ~12 hours). The web UI shows a dismissible banner when a newer version exists, and **Settings → About & updates** lists your version with a manual refresh.

Self-host Docker upgrade:

```bash
docker compose pull && docker compose up -d
```

Disable checks entirely with `PRINT_PARTNER_UPDATE_CHECK=0`. Offline or failed lookups never show an error banner.

### Releasing (maintainers)

Prepare every version-bearing file with the release command, review the dry
run, and commit the result:

```bash
NEXT_VERSION=X.Y.Z
node scripts/release.mjs prepare "$NEXT_VERSION" --dry-run
node scripts/release.mjs prepare "$NEXT_VERSION"
git add CHANGELOG.md Dockerfile README.md OPERATIONS.md docker-compose.yml web/package.json web/package-lock.json web/DEPLOY.md
git commit -m "chore(release): prepare v${NEXT_VERSION}"
node scripts/release.mjs check
git tag -a "v${NEXT_VERSION}" -m "Release v${NEXT_VERSION}"
git push origin main
git push origin "v${NEXT_VERSION}"
```

The `release.yml` workflow first requires the complete web quality suite,
high-severity dependency audit, production Docker smoke test, and manifest
schema/drift validation. It peels the annotated tag to its commit, checks every
version sink, and builds a multi-arch candidate image (`linux/amd64` and
`linux/arm64`) with matching OCI metadata. CI then attaches a digest-pinned
`release-identity.json` to the GitHub Release, creates or verifies the immutable
`:X.Y.Z` alias, verifies the public identity, and moves `:latest` last as a
convenience alias. A conflicting existing candidate, release asset, or version
alias fails instead of being overwritten.

GHCR package visibility is a one-time package setting, not a release step. The
historical `v3.1.0` tag points to disconnected history and its workflow pushed
an image before failing to create a GitHub Release. Do not move, delete, or
reuse that tag. `v3.2.0` is the first release prepared under the repaired
identity contract.

### Local development

```bash
cd web
npm ci
npm run dev   # predev builds @print-partner/contracts and @print-partner/domain first
```

API: `http://127.0.0.1:18765` · Vite UI: `http://127.0.0.1:5173`

The versioned integration API is available at `http://127.0.0.1:18765/api/v1`. See [`../docs/api.md`](../docs/api.md). Connect optional [Spoolman](../docs/integrations/SPOOLMAN.md) filament inventory in **Settings > Integrations**.

## SaaS mode (`DEPLOY_MODE=saas`)

SaaS mode can use **Postgres for app data** when `DATABASE_URL` is set (tenant-scoped rows). The current Postgres repository runs through a synchronous compatibility bridge and is **experimental, not production-ready**: it does not provide native transaction semantics for repository mutations. Production startup fails closed unless `POSTGRES_EXPERIMENTAL=1` explicitly acknowledges this limitation. SQLite remains the supported database. `GET /health` reports `db.support_status` as `supported` or `experimental`.

The bridge also depends on Drizzle's private prepared-field metadata and
`drizzle-orm/utils` result mapper. Dependency updates must pass
`sync-db-bridge.test.ts` and the live Postgres smoke before release. Each
synchronous query is limited to 10,000 returned rows and an 8 MiB serialized
result. Callers must paginate larger reads. These ceilings keep the
child-process protocol bounded and produce an explicit error instead of an
implicit stdout-buffer failure.

File blobs (repos, exports, thumbs) stay on disk under `SAAS_DATA_DIR`.

### Quick local SaaS stack

```bash
docker compose -f docker-compose.saas.yml up --build
```

Includes Postgres 16 and the app with `SAAS_ALLOW_ANONYMOUS=1` and `MULTI_USER=0` for easy dev.

The Compose credentials are explicitly development-only defaults. Before using
the stack on any shared network, set strong values for
`PP_DEV_POSTGRES_PASSWORD` and `PP_DEV_SESSION_SECRET`. Ports bind to
`127.0.0.1` by default. Set `PP_BIND_ADDRESS` explicitly only when a firewall
and authentication protect the shared interface.

### SaaS environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `DEPLOY_MODE` | Yes | Set to `saas` |
| `SAAS_DATA_DIR` | Recommended | Repos, exports, thumbs scratch dir (default `./data`) |
| `DATABASE_URL` | Experimental | Postgres connection string. Runs migrations on startup and requires explicit experimental opt-in in production. |
| `POSTGRES_EXPERIMENTAL` | With production Postgres | Set `1` to acknowledge that the sync bridge is experimental and lacks native repository transactions |
| `MULTI_USER` | Invite host: `1` | `1` enables multiple accounts and sharing. Self-host: the first registered user claims existing data. Hosted planning (`DEPLOY_MODE=saas`) starts empty and does not claim the `default` tenant. |
| `SINGLE_USER_AUTH` | Optional | `1` enables one self-host administrator account without multi-user sharing |
| `SESSION_SECRET` | Multi-user / OAuth / prod | Not needed for `SINGLE_USER_AUTH=1`. Supply it for `MULTI_USER=1` or OAuth in production. |
| `PP_BIND_ADDRESS` | Compose only | Host bind for app and Postgres ports. Defaults to loopback (`127.0.0.1`). |
| `PP_DEV_MULTI_USER` / `PP_DEV_SESSION_SECRET` | Development Compose | Override the single-user mode and development-only session secret |
| `ALLOWED_ORIGINS` | Prod | Comma-separated CORS origins (alias: `CORS_ORIGIN`) |
| `TRUST_PROXY` | Invite host: `1` | Trust `X-Forwarded-*` from the HTTPS reverse proxy. Required when TLS terminates in front of the container. |
| `SAAS_BASIC_AUTH` | Optional | `user:password` for HTTP Basic dev auth |
| `GITHUB_CLIENT_ID` / `SECRET` / `GITHUB_CALLBACK_URL` | OAuth | GitHub OAuth app |
| `DISCORD_CLIENT_ID` / `SECRET` / `DISCORD_CALLBACK_URL` | OAuth | Discord OAuth app (`/auth/discord/callback`) |
| `GOOGLE_CLIENT_ID` | Optional | Public Google OAuth **Web** client id for parts-manifest Drive open/save (SPA GIS + Drive API). It is not a secret and is exposed on `GET /health`. Enable Drive API and add your app origin to Authorized JavaScript origins. Dev SPA fallback: `VITE_GOOGLE_CLIENT_ID`. |
| `SAAS_ALLOW_ANONYMOUS` | Optional | `1` to allow unauthenticated API (dev only). Production hosted planning refuses to start if this is set. |
| `REGISTRATION_OPEN` | Invite host | Set `0` after invites to refuse new email and OAuth registrations. Existing accounts still log in. |
| `REDIS_URL` | Optional | BullMQ-backed job queue for horizontal scaling |

### Invite-beta hosted planning

The public invite host is Library, Builds, Sources, Plan, Production export, and Checkoff. It does not talk to printers on a LAN. `DEPLOY_MODE=saas` denies LAN adapters even if a tenant uses curl.

`docker-compose.saas.yml` stays a development stack. It turns on `SAAS_ALLOW_ANONYMOUS=1` and Postgres. Production hosted planning refuses to start with anonymous access. Do not use that file for the invite host.

#### Checklist

1. Copy `hosted-planning.env.example` to `hosted-planning.env`.
2. Set `DEPLOY_MODE=saas` (already pinned in `docker-compose.hosted.yml`).
3. Set `MULTI_USER=1` (already pinned).
4. Leave `SAAS_ALLOW_ANONYMOUS` unset.
5. Leave `DATABASE_URL` and `POSTGRES_EXPERIMENTAL` unset. SQLite on the volume is the database.
6. Generate a strong `SESSION_SECRET`.
7. Pin `ALLOWED_ORIGINS` to the public HTTPS origin. Do not use `true`.
8. Put HTTPS in front. Terminate TLS at Caddy, nginx, or a load balancer. Proxy to `127.0.0.1:8080`.
9. Keep `SESSION_COOKIE_SECURE=1` and `TRUST_PROXY=1` (already pinned).
10. Leave `REGISTRATION_OPEN=1` while you create invite accounts. Set `REGISTRATION_OPEN=0` and recreate the container after the list is done. Existing accounts still log in.
11. Back up the `print-partner-hosted-data` volume. HTTP `/backups` is off on this host. Tenants cannot restore the instance.

```bash
cp hosted-planning.env.example hosted-planning.env
# edit SESSION_SECRET and ALLOWED_ORIGINS
docker compose --env-file hosted-planning.env -f docker-compose.hosted.yml up -d
```

Health should advertise `hosted_planning` and `invite_board`. It omits `mcp_http`, `backups`, and `plan_sharing`. GitHub and Discord buttons appear only when health lists `github_oauth` or `discord_oauth`. Leave Discord unset.

Signed-in invitees share on the Board at `/board`. A post stores a frozen `printpartner-reference-share` Build snapshot. Later edits to the author's Build do not change the post. Comments are a flat list. Add to my Builds maps each reference to a Library Source the invitee already has and creates a Build only when every included file is Ready. Kit inbox, send-to-user copies, and `.print-partner-kit` export are 403 on this host. `GET /plans/:id/reference-share` stays available for the post preview.

Volume backup from the Docker host:

```bash
docker run --rm \
  -v print-partner-hosted-data:/data:ro \
  -v "$PWD":/backup \
  alpine:3.21.3 \
  tar czf /backup/print-partner-hosted-$(date -u +%Y%m%d).tar.gz -C /data .
```

The operator still owns DNS, the certificate, the invite list, closing registration, and storing that archive off the live volume. This cut does not add a local relay, Prusa Connect, Moonraker cloud, a native app, or Postgres.

The following claim-path note is for self-host `MULTI_USER` upgrades. Hosted planning starts empty and does not claim the `default` tenant.

Print Partner now gives a first multi-user account direct ownership of the
bootstrap `default` tenant. No application tables or data directories move
during that claim. If an installation first enabled `MULTI_USER` while running
v3.3.0, keep an offline backup before upgrading. That version could move only
part of an existing tenant. The upgrade repairs an active Source ownership
mismatch only when the result is unambiguous. It does not generally recover or
re-tenant inactive Source history, and it stops startup for manual recovery when
a protected revision conflicts. Review ownership on every installation that ran
the v3.3 claim path. Split or conflicting Source documents and notes, Plan drafts
and revisions or their dependent history, accepted Plates, slicer and printer
configuration, print jobs, telemetry, or events need graph-aware tenant
recovery.

### Auth routes

| Route | Description |
|-------|-------------|
| `GET /auth/github` | Start GitHub OAuth |
| `GET /auth/callback` | GitHub OAuth callback |
| `GET /auth/discord` | Start Discord OAuth |
| `GET /auth/discord/callback` | Discord OAuth callback |
| `POST /auth/register` | Email + password registration. Single-user mode closes registration after its administrator exists. `REGISTRATION_OPEN=0` closes registration for every remaining account type. |
| `POST /auth/login` | Email + password login |
| `POST /auth/forgot-password` | Request a password reset email |
| `POST /auth/reset-password` | Set a new password using a reset token |
| `POST /auth/change-password` | Change password while signed in |
| `POST /auth/logout` | Clear session |
| `GET /auth/me` | Current user + tenant |
| `POST /auth/dev-login` | Dev session helper |
| `GET /board/posts` | Newest-first invite Board feed (session required) |
| `GET /board/posts/:id` | Frozen recipe snapshot and flat comments |
| `POST /board/posts` | Post a Build snapshot with a caption |
| `POST /board/posts/:id/hide` | Admin hides a post |
| `POST /board/posts/:id/comments` | Add a comment |
| `DELETE /board/comments/:id` | Author or admin deletes a comment |
| `POST /plans/:id/shares` | Send build copy to another user. 403 on the invite host. |
| `GET /shares/incoming` | List pending shares for current user. 403 on the invite host. |
| `POST /shares/:token/accept` | Import shared build as new plan. 403 on the invite host. |
| `DELETE /shares/:id` | Revoke a pending share. 403 on the invite host. |

### Password reset email (multi-user)

When `MULTI_USER=1`, users can reset forgotten passwords from **Sign in > Forgot password?** or `POST /auth/forgot-password`.

| Variable | Required | Description |
|----------|----------|-------------|
| `SMTP_HOST` | For email delivery | SMTP server hostname |
| `SMTP_PORT` | Optional | Default `587` (`465` implies TLS) |
| `SMTP_USER` / `SMTP_PASS` | Optional | SMTP credentials when required by your provider |
| `SMTP_FROM` | With `SMTP_HOST` | From address (e.g. `Print Partner <noreply@example.com>`) |
| `SMTP_SECURE` | Optional | Set `1` for implicit TLS (typical on port 465) |
| `APP_PUBLIC_URL` | With `SMTP_HOST` | Canonical public app URL for reset links (for example, `https://print.example.com`) |
| `PASSWORD_RESET_DEV_EXPOSE` | Dev only | When SMTP is not configured, non-production mode returns `dev_reset_url` in the API response and logs the link. Set `0` to disable. |

Without SMTP in production, reset requests are accepted but no email is sent. Configure SMTP for production deployments.

### Data migration from desktop

```bash
cd web
npx tsx scripts/import-sqlite.ts \
  --source-db ~/.print-partner/print-partner.db \
  --source-repos ~/.print-partner/repos \
  --dest ./data
```

### Exports and imports (web / Docker)

- **Exports:** job endpoints write files under `exports/` in the data dir and return `download_url` (e.g. `/exports/Plan/checklist.html`). The UI triggers a browser download via `GET /exports/*` with `Content-Disposition: attachment`.
- **Kit bundle import (browser):** upload `.print-partner-kit.zip` with `POST /imports/kit-bundle` (multipart field `file`). Command palette **Import shared build...** uses this path.
- **Kit bundle import (server host):** `POST /admin/import-kit-bundle` with `{ "path": "..." }` when the `.print-partner-kit` file already exists under the data directory (admin scripts on the same machine as the engine).
- **Source ZIP import:** `POST /sources/:id/upload-zip` (multipart upload only).

### Smoke test

Run `npm run quality` before deploying a changed image.

## Known gaps

- No PyVista/VTK mesh preview server-side (client thumbnails only)
- Printables/MakerWorld source create returns `501`
- Community manifest submission draft export and Source/Plan maintenance and manifest reports return explicit HTTP 501 unsupported responses. Source manifest presence reads owned canonical file availability. These GRE-292 corrections are unreleased; see [manifest API availability](../docs/gre-292-manifest-api-availability.md).
- Bambu metadata in 3MF is minimal (plate JSON + slice_info stub)
