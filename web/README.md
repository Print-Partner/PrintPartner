# Print Partner web platform

TypeScript monorepo for the browser-hosted Print Partner.

## Layout

| Package | Path | Role |
|---------|------|------|
| `@print-partner/web` | `apps/web` | Vite + React SPA |
| `@print-partner/ui` | `apps/ui` | Replacement UI, Rust core only. See [its README](apps/ui/README.md) |
| `@print-partner/server` | `apps/server` | Fastify API |
| `@print-partner/contracts` | `packages/contracts` | Shared API types |
| `@print-partner/domain` | `packages/domain` | Domain logic |

## Quick start

```bash
cd web
npm ci
npm run dev
```

- **API** `http://127.0.0.1:18765` (`/health`)
- **UI** `http://127.0.0.1:5173`

## Deploy modes

| Mode | App DB | Files | Auth |
|------|--------|-------|------|
| **self-host** | SQLite under `PRINT_PARTNER_DATA_DIR` | Local disk | Optional Basic |
| **saas** hosted planning | SQLite under `SAAS_DATA_DIR` | Local disk, 2 GiB tenant quota | `MULTI_USER=1`. No anonymous in production. |
| **saas** + `DATABASE_URL` | Experimental Postgres sync bridge (tenant-scoped) | `SAAS_DATA_DIR` | OAuth / Basic / dev anonymous |

Postgres is not production-ready because the synchronous compatibility bridge
does not provide native repository transactions. Production startup requires
`POSTGRES_EXPERIMENTAL=1`; SQLite remains the supported database. See
[DEPLOY.md](./DEPLOY.md) for Docker Compose, credentials, and environment
variables.

## Test changes

```bash
cd web
npm test
```

Run `npm run quality` before opening a pull request. It runs linting, type checks, unit tests, workflow smoke tests, the production build, and browser tests.

## Changelog

See the repository [CHANGELOG.md](../CHANGELOG.md).
