# Install Print Partner

This guide installs the self-hosted Docker setup on macOS, Windows, or Linux. The container serves the web app and API on port 8080. SQLite data and synced files stay in a Docker volume by default.

## Requirements

- Docker Engine with Compose v2, or Docker Desktop
- A browser. Other computers on the local network can open `http://<this-computer>:8080`. Set `PP_BIND_ADDRESS=127.0.0.1` to keep the port on this computer, and enable authentication on an untrusted network.
- Git, unless you download the repository as a zip file

Verify Docker before continuing:

```bash
docker --version
docker compose version
```

Both commands must succeed. Install Docker from the [official Docker documentation](https://docs.docker.com/get-docker/) if either command is missing.

## Download Print Partner

```bash
git clone https://github.com/Print-Partner/PrintPartner.git
cd PrintPartner
```

If you do not use Git, download the source archive from the [Print Partner repository](https://github.com/Print-Partner/PrintPartner), extract it, and open that directory in a terminal.

## Start the container

Pull the published image and run it in the background:

```bash
docker compose pull
docker compose up -d
```

On first startup of each new release, Print Partner creates and validates a database backup in `/data/backups` before it runs migrations. If backup creation or validation fails, the server does not modify the database or start. Repeated starts of the same release reuse the existing pre-update backup.

Open [http://localhost:8080](http://localhost:8080), or `http://<this-computer>:8080` from another machine. To keep the port on this computer only: `PP_BIND_ADDRESS=127.0.0.1 docker compose up -d`. Enable authentication if the network is not trusted. Check the service if the page does not load:

```bash
docker compose ps
docker compose logs -f print-partner
```

To build the image from source instead:

```bash
docker compose up --build -d
```

## Open it from another computer

Find the Docker host's LAN address, then open `http://<host-address>:8080` from another device on the same network. Allow inbound TCP port 8080 through the host firewall if needed.

Do not expose the default installation directly to the public internet. Use HTTPS, authentication, and a reverse proxy for remote access. Set `PRINT_PARTNER_API_KEY` before connecting an MCP client from another machine.

The private invite host is a different Compose file. See [Invite-beta hosted planning](../web/DEPLOY.md#invite-beta-hosted-planning). That path is planning and export. Live send stays on this self-host install.

## Create the first build

1. Open **Library** and add a GitHub repository, local folder, or zip file.
2. Sync the source and confirm its import rules.
3. Select **New Build** and enter a name.
4. Attach the source and select STL files on **Sources**.
5. Review quantities and warnings on **Plan**, then apply the draft.
6. Track printed units on **Checkoff**.
7. Arrange plates and export or send files from **Production**.

## Data storage

The default Compose file mounts the `print-partner-data` volume at `/data`. It contains:

- the SQLite database
- synced source repositories
- generated thumbnails
- exports and job artifacts

Stopping the container does not delete the volume:

```bash
docker compose down
```

Never add `-v` unless you intend to delete the named volume.

Inspect disk use from inside the container. Compose prefixes named volumes with the project name, so the host volume is usually `<project>_print-partner-data`, not `print-partner-data`:

```bash
docker compose exec print-partner du -sh /data
```

See [Operations](../OPERATIONS.md) for backup and restore commands.

## Run with a host directory

The named volume is the simplest option. To use a host bind mount, create the directory and give uid 1000 access:

```bash
sudo mkdir -p /srv/print-partner
sudo chown 1000:1000 /srv/print-partner
sudo chmod 750 /srv/print-partner
```

Replace the service volume in `docker-compose.yml`:

```yaml
services:
  print-partner:
    volumes:
      - /srv/print-partner:/data
```

The image starts as root only long enough to prepare `/data`, then runs the Node process as `ppuser` with uid 1000. Do not set `user: "1000:1000"` for an unprepared named volume because the entrypoint would be unable to fix its ownership.

Verify the running process:

```bash
docker top "$(docker compose ps -q print-partner)" -eo uid,user,pid,cmd
```

The Node process should show uid 1000.

## Update

If this install currently binds only to this computer and you want to keep that,
put `PP_BIND_ADDRESS=127.0.0.1` in `.env` before the next `docker compose up -d`.
Otherwise 8080 is published on every host interface.

Pull repository changes and the new image:

```bash
git pull --ff-only
docker compose pull
docker compose up -d
```

If you build locally, replace `docker compose pull` with:

```bash
docker compose build --pull
```

Create and download a full backup before a storage change or major import. The automatic pre-update backup contains the database. A full backup also contains stored source files and generated assets.

## Change the port

Edit the port mapping in `docker-compose.yml`. This example exposes the app on port 9090:

```yaml
ports:
  - "9090:8080"
```

Restart the container and open `http://localhost:9090`.

## Troubleshooting

### Port 8080 is already in use

Change the host-side port as shown above, or stop the process that owns port 8080.

### Docker reports permission denied for `/data`

Remove any `user:` override from the Compose service. For a bind mount, set its owner to uid 1000 and restart:

```bash
sudo chown -R 1000:1000 /srv/print-partner
docker compose up -d
```

### The image pull is denied

Build from source while the package is unavailable:

```bash
docker compose up --build -d
```

### A browser route returns JSON

A reverse proxy may be rewriting request headers. Browser navigation must preserve `Sec-Fetch-Mode: navigate` or an `Accept` header containing `text/html`. Open the container directly at `http://localhost:8080` to isolate the proxy.

### Another device cannot connect

Confirm that the container is running, use the host's LAN address instead of `localhost`, and allow inbound TCP 8080 through the host firewall.

### Windows Docker does not start

Docker Desktop uses WSL 2. Confirm that WSL 2 is enabled and Docker Desktop integration is active for your Linux distribution.

## Next steps

- [Documentation index](README.md)
- [Deployment reference](../web/DEPLOY.md)
- [Operations](../OPERATIONS.md)
- [MCP setup](assistant-mcp.md)
- [Printer setup](integrations/PRINTER_SETUP.md)
