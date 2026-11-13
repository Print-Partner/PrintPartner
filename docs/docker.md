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

1. `web` (Node 24.21.0): `npm ci`, then the desktop web build (`VITE_PRINT_PARTNER_DESKTOP=1 npm run build`). `rust/scripts/build-desktop-manifest.py` then builds the replacement UI (`@print-partner/ui`) and writes the measured release manifest, which lists `apps/ui/dist` as `replacement_frontend`. The stage fails if `web/apps/ui/dist/index.html` is missing.
2. `rust` (Rust 1.99.0): `cargo build --release -p pp-server --features desktop_rebuild`, with that manifest embedded through `PP_DESKTOP_RELEASE_MANIFEST`.
3. `runtime` (Debian bookworm-slim): the `pp-server` binary, the measured replacement UI (`apps/ui/dist`, which it serves), the measured old frontend (`apps/web/dist`, still shipped), the community manifests (`apps/server/dist/current/data/manifests`), `web/package.json` for the version, `ca-certificates`, `tini`, and the license and notice files in `/usr/share/doc/print-partner/`.

The runtime stage has no Node, npm, npx, Python, GUI or slicer. `scripts/docker-rust-smoke.py` checks this. Every base image is pinned by tag and digest. Release builds of the Node image pass `PP_APP_VERSION` and `PP_BUILD_DATE` the same way, and the Rust-only image takes the same build arguments for its OCI labels.

### Release check without Node

A Rust-only start checks the release without Node. When `web/apps/ui/dist` is installed, `pp-core` calls `pp_compat::snapshot_rust_only_release`, which checks the `replacement_frontend` inventory and keeps the verified bytes for serving. When it is absent, as in an image built before GRE-1387, `pp-core` calls `pp_compat::verify_rust_only_legacy_release`, which checks the `frontend` inventory instead, and writes `Replacement UI bundle absent` to stderr once the data directory is admitted. A refused start writes only its refusal. Both check:

- The runtime version and commit match the manifest embedded in the binary.
- The selected frontend's file list and digests match exactly.
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
- **Port.** This image sets `PP_LISTEN_ADDR=0.0.0.0:8080`. Docker publishes it on the host's loopback interface in this example. Open `http://127.0.0.1:8080` after obtaining a bootstrap URL (see [Sign in](#sign-in)). For a different published port, LAN address or specific-IP bind, replace the URL's host and port with that reachable address, keeping its path and query private.
- **Admission.** `PP_LISTEN_ADDR` takes an IP address and port; malformed values fail startup. It requires the Rust-only runtime. Without it, the server keeps the saved loopback port. Desktop startup never reads this setting. Requests in explicit listener mode require a literal-IP Host; DNS names, forwarded headers and cross-site requests are refused. Browser mutations require an Origin matching that Host, including the published port.
- **LAN exposure.** Publishing with `-p 8080:8080` exposes the listener on host network interfaces. Bootstrap admission and account login still apply, and MCP needs an API key ([Connect an MCP client](#connect-an-mcp-client)). Plain HTTP does not encrypt credentials, sessions or API keys. Use the loopback-only mapping above for local use. This setting does not enable desktop network sharing.
- **Upgrading from host networking.** This image keeps `PP_LISTEN_ADDR=0.0.0.0:8080` so port publishing works without an extra environment variable. The old `--network host` command now listens on every host interface over plain HTTP. Replace that command with the `-p 127.0.0.1:8080:8080` command above for local-only host access. To keep host networking on loopback only, add `-e PP_LISTEN_ADDR=127.0.0.1:8080`:

  ```sh
  docker run -d --name print-partner --network host \
    -e PP_LISTEN_ADDR=127.0.0.1:8080 \
    --restart unless-stopped \
    --read-only --tmpfs /tmp:size=1g,mode=1777 \
    --cap-drop ALL --security-opt no-new-privileges \
    -v pp-data:/data \
    pp-server:test
  ```

- **Container networks.** Other containers on the same Docker bridge or Compose network can reach port 8080 without any `-p` mapping. A loopback host mapping does not isolate those containers. `-P` publishes exposed ports on all host interfaces; use the explicit loopback mapping above instead.
- **Rate limits.** When docker-proxy forwards connections, every client appears as the proxy's IP address and shares one rate-limit bucket. Forwarded headers do not restore client identities; this listener refuses them.

## Sign in

A browser gets in through a single-use sign-in URL. Ask the running server for one:

```sh
docker exec print-partner pp-server bootstrap-url --data /data
```

The command prints `http://127.0.0.1:<port>/__desktop/bootstrap?token=<64 hex digits>`. For a different published port, LAN address or specific-IP bind, replace its host and port with the reachable IP address; keep the path and query private. Open it within 60 seconds. It works once, then sets the session cookie. Run the command again for a new URL; at most four unused URLs stay valid at a time, and a fifth replaces the oldest. The replacement UI then opens its setup screen to create the owner account on first run, and its sign-in screen later whenever the browser has no valid session ([GRE-1429](https://linear.app/greetz/issue/GRE-1429)).

- **Where the URL goes.** Only to the terminal that ran the command. `pp-server` does not print or log it, and it is not in `docker logs` or the status file.
- **Who can get one.** Only the account that owns `/data`, UID 1000 in this image. `docker exec` runs as that user by default. `pp-server` hands the URL over a Unix socket, `/data/.pp-server-bootstrap.sock`, with mode 0600, and it also checks the connecting process's UID. Another UID, root included (`docker exec -u 0`), is refused. The command also refuses a directory the caller does not own or that other accounts can write, and a socket whose server runs as another account. With a host bind mount for `/data` and no user-namespace remapping, a host process running as the data directory's UID can also reach the socket and get a URL. That is by design: that account already owns the data.
- **No server running.** The command exits 1, prints `bootstrap-url: No pp-server is running on /data` to stderr, and changes nothing. If minting fails for another reason, the command says `pp-server could not create a sign-in URL; its stderr says why`, and `pp-server` writes the reason, never a URL, to its stderr.
- **The socket.** Only a Rust-only start (`PP_DESKTOP_REBUILD=1`, as in this image) creates it, after startup, and removes it on shutdown. A socket left by a crashed server is replaced at the next start. If something else exists at that name, `pp-server` leaves it, prints `pp-server bootstrap-url is unavailable` to stderr, and serves without headless sign-in. The same happens when the socket path is too long for a Unix socket name: 107 bytes on Linux, 103 on macOS. `/data` is far below either.

## Connect an MCP client

MCP answers at `/mcp` and `/api/v1/mcp` on the published port, for example `http://127.0.0.1:8080/api/v1/mcp`. Every MCP request needs an API key; the sign-in cookie is not one. A valid key is admitted from any address the listener admits, a LAN peer included, through the same key guard as a desktop loopback start. No address is trusted without a key.

Make a key while signed in. The replacement UI has no key screen yet, so use `curl` and `jq` with a sign-in URL and your account:

```sh
url=$(docker exec print-partner pp-server bootstrap-url --data /data)
curl -s -c pp.jar -o /dev/null "$url"
read -r -p 'Email: ' email; read -r -s -p 'Password: ' password; echo
email="$email" password="$password" jq -n '{email: $ENV.email, password: $ENV.password}' |
  curl -s -b pp.jar -c pp.jar -H 'Origin: http://127.0.0.1:8080' -H 'Content-Type: application/json' \
    --data @- http://127.0.0.1:8080/auth/login
unset password
curl -s -b pp.jar -H 'Origin: http://127.0.0.1:8080' -H 'Content-Type: application/json' \
  -d '{}' http://127.0.0.1:8080/settings/api-keys
rm pp.jar
```

The password stays out of shell history and every process's argument list. The last command prints the key once, as `"key":"ppk_..."`; keep it secret and clear the terminal if others can see it. `GET /settings/api-keys` lists keys and `DELETE /settings/api-keys/{id}` revokes one, with the same sign-in cookies.

Configure the MCP client with the URL and the header `Authorization: Bearer ppk_...` (or `X-Print-Partner-API-Key: ppk_...`). The client sends no Origin and no cookie. Answers:

- A valid key opens an MCP session; `tools/list` and the read tools work.
- No key, or a key that is unknown, revoked or expired: `401` with `{"detail":"Valid API key required"}`, and no session. A POST or DELETE with neither a key nor an Origin is refused earlier with `403`, and so is any request whose Origin does not match the listener, even with a valid key.
- Host, Origin, forwarded headers and DNS names follow the [Admission](#run) rules above. MCP also has its own budget of 1000 requests a minute ([Assistant MCP](assistant-mcp.md)).

Over a LAN, plain HTTP sends the key unencrypted. Prefer the loopback-only mapping, or reach the host through an encrypted tunnel.

## Volumes and the data directory

`/data` holds the SQLite database (`print-partner.db` with its WAL), the ownership marker `.pp-rust-owned.json`, the lock `printpartner.lock`, the sign-in socket `.pp-server-bootstrap.sock` while the server runs, and the `sources`, `repos`, `covers`, `thumbs` and `exports` folders. `desktop.toml` appears only on a volume created by an image without `PP_LISTEN_ADDR`. This image ignores its saved port, and a fresh volume started from this image has no `desktop.toml`. Use a named volume or a local directory. Do not use a network filesystem.

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

Signing out (`POST /auth/logout`) revokes the account session and clears its cookie; `pp-server` keeps serving. The browser keeps its admission from the sign-in link, so signing in again (`POST /auth/login`) needs no restart and no new `pp-server bootstrap-url`. The old session authorizes no new request; an event or printer stream it opened ends at its next session check, within about 15 seconds. On the desktop app, signing out still ends the runtime.

## What the Rust-only image cannot do yet

- **No DNS names or reverse proxy.** The explicit listener accepts literal IP addresses only. Desktop [Network sharing](network-sharing.md) remains unimplemented and off by default.
- **MCP has keys only.** MCP needs an API key ([Connect an MCP client](#connect-an-mcp-client)). There is no OAuth, no per-tool permission, no TLS and no key screen in the replacement UI yet.
- **No GitHub or Discord sign-in.** These providers are unavailable on the explicit listener: their callback URL is built from the loopback origin, and the listener refuses the provider's cross-site redirect back to `/auth/callback`. This image configures neither provider ([GRE-974](https://linear.app/greetz/issue/GRE-974)); sign in with the owner account's email and password.
- **amd64 only, locally.** The Dockerfile has no architecture-specific steps, but only amd64 has been built and smoke-tested. No workflow builds, signs or publishes this image, and it has no SBOM or provenance attestation yet.

## Smoke test

```sh
python3 scripts/docker-rust-smoke.py pp-server:test
```

The script needs Linux and Docker. It checks that the runtime has no Node, npm, npx or Python. It checks that the image user is 1000:1000, that `PP_DESKTOP_REBUILD=1` is set, and that `pp-server healthcheck --help` works. It then starts the image hardened as in [Run](#run), on a new named volume. Both probes must pass, and Docker must report the container healthy. The script signs in with `docker exec <container> pp-server bootstrap-url --data /data`. The URL must work once and be refused the second time, its token must not appear in `docker logs`, and the same command as root (`docker exec -u 0`) must fail. With that sign-in, `GET /` must return the image's `apps/ui/dist/index.html` with one style nonce element added, and `docker logs` must not contain `Replacement UI bundle absent`. Its CSP `connect-src` must name `ws://127.0.0.1:<published port>/ws/printers`, the authority the browser used. The script then registers an account through the API. It then stops the container with SIGTERM, which must exit 0 with a complete receipt while only `tini` and `pp-server` ran. It starts a second container on the same volume and logs in with the stored account. The script reaches /health and sign-in through the published host loopback port over the Docker bridge. It verifies non-loopback login requirements. Over the published port, from a non-loopback peer, it makes an API key with the signed-in session, connects to `/mcp` and `/api/v1/mcp` with it, lists tools and runs `list_plans`, and checks that no key or a wrong key gets `401` (`403` without an Origin either) and no session. It records the bound address 0.0.0.0:8080. It never prints a sign-in URL or cookie. It still reads the test-only credential file to check that no Node child started. It removes its containers and volume on exit.

## Importing Node data

Node and Rust never share a live data directory. The whole-runtime cutover procedure in [Architecture](architecture.md#whole-runtime-cutover-and-rollback) applies: stop Node, back up, migrate a separate Rust-owned copy, and keep the pre-cutover copy. `pp-server import --from <folder or archive> --data <new folder>` copies a stopped Node data folder or a full backup archive into a new Rust-owned folder, and `--dry-run` reports first what it would convert. The import is not complete yet: its report says `complete: false`, and such a copy is disposable. See [Node import](node-import.md).

## To be written

- `docker compose` file for the Rust-only image.
- Upgrade, backup and restore.
