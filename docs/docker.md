# Docker image

Two images exist:

- The **Node image** (`Dockerfile`) is the released image. It runs the Node server. [Install](INSTALL.md) describes it.
- The **Rust-only image** (`Dockerfile.rust`) runs `pp-server` with the `desktop_rebuild` feature compiled in and `PP_DESKTOP_REBUILD=1`. No release workflow builds or publishes it yet. It is a try-it build, and the [limits below](#what-the-rust-only-image-cannot-do-yet) apply.

The rest of this page describes the Rust-only image.

## Scope

- One user. No multi-user accounts, hosted SaaS mode or Postgres.
- The image runs `pp-server`. There is no launchd, systemd or Windows Service mode.
- `PP_DESKTOP_REBUILD=1` is set in this image only. Every other build keeps the Cargo feature and the runtime switch off ([Desktop rebuild flag](desktop-rebuild.md)).

## Build

```sh
docker build -f Dockerfile.rust --build-arg PP_COMMIT="$(git rev-parse HEAD)" -t pp-server:test .
```

Without `PP_COMMIT`, the build records the all-zero commit. That is an unversioned local build.

The build has three stages:

1. `web` (Node 24.21.0): `npm ci`, then the desktop web build (`VITE_PRINT_PARTNER_DESKTOP=1 npm run build`). `rust/scripts/build-desktop-manifest.py` then writes the measured release manifest.
2. `rust` (Rust 1.99.0): `cargo build --release -p pp-server --features desktop_rebuild`, with that manifest embedded through `PP_DESKTOP_RELEASE_MANIFEST`.
3. `runtime` (Debian bookworm-slim): the `pp-server` binary, the measured frontend (`apps/web/dist`), the community manifests (`apps/server/dist/current/data/manifests`), `web/package.json` for the version, `ca-certificates`, `tini`, and the license and notice files in `/usr/share/doc/print-partner/`.

The runtime stage has no Node, npm, npx, Python, GUI or slicer. `scripts/docker-rust-smoke.py` checks this. Every base image is pinned by tag and digest. Release builds of the Node image pass `PP_APP_VERSION` and `PP_BUILD_DATE` the same way, and the Rust-only image takes the same build arguments for its OCI labels.

### Release check without Node

A Rust-only start checks the release with `pp_compat::verify_rust_only_release`. It checks:

- The runtime version and commit match the manifest embedded in the binary.
- The frontend's file list and digests match exactly.
- The community manifests' file list and digests match exactly.

It does not measure the Node executable, the Node backend, the workspace packages or `node_modules`, so `pp-server` needs no `--node` in this mode. The Node-backed start still runs the full `verify_bundle` and still requires `--node`.

## Run

```sh
docker volume create pp-data
docker run -d --name print-partner \
  -p 127.0.0.1:8080:8080 \
  --restart unless-stopped \
  --read-only --tmpfs /tmp:size=1g,mode=1777 \
  --cap-drop ALL --security-opt no-new-privileges \
  -v pp-data:/data \
  pp-server:test
docker logs print-partner    # bound address and local probe origin
```

- **User.** The image runs as UID 1000, GID 1000 (`ppuser`) from the first instruction. It never starts as root and has no `gosu` or entrypoint `chown`.
- **Writes.** Installation data goes to `/data` only. `pp-server` also writes its storage runtime directory and its status file to `/tmp`. Each start's storage preflight copies the database and its WAL into `/tmp` twice, one after the other, and deletes each copy. Backup validation also extracts there. Mount `/tmp` as a bounded tmpfs at least twice the size of `print-partner.db` plus its WAL, with 64 MB to spare. The example's 1 GB fits a database up to about 450 MB. A tmpfs uses memory only for what it holds. With that tmpfs mounted, the root filesystem can be read-only (`--read-only`).
- **Capabilities.** `pp-server` needs no Linux capabilities. Drop all of them with `--cap-drop ALL`, and use `--security-opt no-new-privileges`. The image has no setuid helper that needs either.
- **Process tree.** `tini` is PID 1 and forwards signals. `pp-server` is its only child. In Rust-only mode `pp-server` starts no child process.
- **Port.** This image sets `PP_LISTEN_ADDR=0.0.0.0:8080`. Docker publishes it on the host's loopback interface in this example. Open `http://127.0.0.1:8080` after obtaining a bootstrap URL (see the sign-in limitation below). If the published port differs, replace the URL's host and port, keeping its path and query private.
- **Admission.** `PP_LISTEN_ADDR` takes an IP address and port; malformed values fail startup. It requires the Rust-only runtime. Without it, the server keeps the saved loopback port. Desktop startup never reads this setting. Requests in explicit listener mode require a literal-IP Host; DNS names, forwarded headers and cross-site requests are refused. Browser mutations require an Origin matching that Host, including the published port.
- **LAN exposure.** Publishing with `-p 8080:8080` exposes the listener on host network interfaces. Bootstrap admission and account login still apply; MCP stays disabled. Plain HTTP does not encrypt credentials or sessions. Use the loopback-only mapping above for local use. This setting does not enable desktop network sharing.

## Volumes and the data directory

`/data` holds the SQLite database (`print-partner.db` with its WAL), the ownership marker `.pp-rust-owned.json`, `desktop.toml`, the lock `printpartner.lock`, and the `sources`, `repos`, `covers`, `thumbs` and `exports` folders. Use a named volume or a local directory. Do not use a network filesystem.

The storage writer opens only an empty directory or one that already carries the ownership marker. It refuses a non-empty directory without the marker and leaves it unchanged. So a Node image volume cannot be mounted directly; see [Importing Node data](#importing-node-data).

### Volume ownership

A new named volume takes the image's `/data` ownership (1000:1000, mode 0700). A bind mount keeps the host directory's ownership, so make it writable by UID 1000 first:

```sh
mkdir -p ./pp-data && sudo chown 1000:1000 ./pp-data && chmod 0700 ./pp-data
```

To run under another UID, pass `--user UID:GID` and give `/data` that owner.

To change an existing volume's owner, for example a volume a different UID wrote, run a one-off root container with the same image and no network. Nothing else should be using the volume:

```sh
docker stop print-partner
docker run --rm --network none --user 0:0 --entrypoint chown \
  -v pp-data:/data pp-server:test -R 1000:1000 /data
```

This is the only step that runs as root, and it is an explicit, one-time command.

## Health

`pp-server --status-file PATH` keeps a small JSON file. It holds the process ID, a state (`starting`, `ready` or `stopping`) and, once ready, a local probe origin. The file is written atomically and removed on exit. The image passes `--status-file /tmp/pp-server.status.json`.

`pp-server healthcheck <liveness|readiness> --status-file PATH [--timeout-ms N]` reads that file:

| Probe | Passes when |
| --- | --- |
| `liveness` | The recorded process is alive. Once the state is `ready`, the gateway must also answer. Passes while `starting` and `stopping`. |
| `readiness` | The state is `ready` and the gateway answers. Fails while `starting`, which covers the storage lease, the ownership marker decision and schema migrations. Also fails while `stopping`. |

"The gateway answers" means one `GET /health` with no cookie to the recorded local origin, which must return an HTTP status below 500 within the timeout (default 2000 ms, at most 10000 ms). The gateway refuses a request without a session with 401 before admitting it, so the probe creates no session, job or printer contact. The probe reads no secret. A missing, malformed or oversized (over 4 KiB) status file fails both probes. The command exits 0 on pass and 1 on fail.

The image's `HEALTHCHECK` runs `readiness` every 30 s, with a 5 s timeout, a 120 s start period for migrations, and 3 retries.

## Stop

`docker stop` sends SIGTERM. `pp-server` sets the status file to `stopping`, drains the gateway, shuts the storage writer down, prints its shutdown receipt as the last log line, and exits 0. Mutations get up to 5 seconds to drain. The smoke test stops with the default 10-second timeout.

`pp-server` registers its signal handlers after startup finishes. A SIGTERM during startup (lease, preflight, migrations) ends the process at once with exit 143 and no drain; SQLite transactions keep the database consistent, and the next start recovers.

Logging out (`POST /auth/logout`) also stops `pp-server`, as it stops the desktop runtime. The container then exits 0. `--restart unless-stopped` starts it again.

## What the Rust-only image cannot do yet

- **No DNS names or reverse proxy.** The explicit listener accepts literal IP addresses only. Desktop [Network sharing](network-sharing.md) remains unimplemented and off by default.
- **No headless sign-in.** The gateway admits a browser only through a single-use, 60-second bootstrap URL. The desktop app opens that URL. `pp-server` gives it only to tests (`--test-credential-file`). Until a headless sign-in path exists, a browser cannot open the UI from this image.
- **MCP is off.** The gateway answers 403 to `/mcp` and `/api/v1/mcp` in Rust-only mode, whatever key a client sends.
- **The old UI.** The image serves the measured desktop build of `apps/web`, the UI the Rust-only desktop serves. The replacement UI (`apps/ui`) is not served by any Rust runtime yet.
- **amd64 only, locally.** The Dockerfile has no architecture-specific steps, but only amd64 has been built and smoke-tested. No workflow builds, signs or publishes this image, and it has no SBOM or provenance attestation yet.

## Smoke test

```sh
python3 scripts/docker-rust-smoke.py pp-server:test
```

The script needs Linux and Docker. It checks that the runtime has no Node, npm, npx or Python. It checks that the image user is 1000:1000, that `PP_DESKTOP_REBUILD=1` is set, and that `pp-server healthcheck --help` works. It then starts the image hardened as in [Run](#run), on a new named volume. Both probes must pass, and Docker must report the container healthy. The script registers an account through the API. It then stops the container with SIGTERM, which must exit 0 with a complete receipt while only `tini` and `pp-server` ran. It starts a second container on the same volume and logs in with the stored account. The script reaches `/health` and the sign-in page through a published loopback host port over the Docker bridge. It checks that a non-loopback peer cannot read Plans without account login or use MCP without a key. It records the bound address `0.0.0.0:8080`. It uses the test-only credential file for the bootstrap URL and never prints it. It removes its containers and volume on exit.

## Importing Node data

Node and Rust never share a live data directory. The whole-runtime cutover procedure in [Architecture](architecture.md#whole-runtime-cutover-and-rollback) applies: stop Node, back up, migrate a separate Rust-owned copy, and keep the pre-cutover copy. No supported tool imports a Node volume into this image yet.

## To be written

- `docker compose` file for the Rust-only image.
- Upgrade, backup and restore.
