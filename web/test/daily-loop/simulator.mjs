// The Moonraker simulator (web/test/moonraker-sim) as run.mjs starts it beside the
// Rust-only container (GRE-1531, architect ruling on GRE-1528): a second container on the
// run's own Docker network, named `pp-sim` on that network, with only its control port
// published, on host loopback.
import { randomBytes } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

export const SIM_ALIAS = "pp-sim";
export const PRINTER_PORT = 7125;
export const CONTROL_PORT = 7200;
/** The printer's address as the product container sees it. */
export const PRINTER_URL = `http://${SIM_ALIAS}:${PRINTER_PORT}`;

const root = resolve(import.meta.dirname, "../../..");

/**
 * Runs `use(keyFile)` with a new random printer API key in a mode-0600 file `api-key`, in a
 * new temporary folder named from `prefix`. The folder is removed when `use` settles,
 * whether it resolved or threw.
 */
export async function withKeyFile(prefix, use, parent = tmpdir()) {
  const dir = mkdtempSync(join(parent, prefix));
  try {
    const file = join(dir, "api-key");
    writeFileSync(file, `${randomBytes(24).toString("hex")}\n`, { mode: 0o600 });
    return await use(file);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The pinned Node image of Dockerfile.rust's `web` stage, which the image build pulls. */
export function nodeImage(dockerfile = readFileSync(resolve(root, "Dockerfile.rust"), "utf8")) {
  const from = /^FROM\s+(node:\S+@sha256:[0-9a-f]{64})\s+AS\s+web\s*$/m.exec(dockerfile);
  if (!from) throw new Error("Dockerfile.rust has no pinned `FROM node:<tag>@sha256:<digest> AS web` line");
  return from[1];
}

/**
 * `docker run` arguments for the simulator. `keyDir` holds the file `api-key`; the key is
 * read from it inside the container and never appears in these arguments. `user` is the
 * host's uid:gid, so the container can read that file while it stays mode 0600.
 */
export function simulatorRunArgs({ container, network, image, keyDir, user }) {
  return [
    ...["run", "-d", "--name", container, "--user", user],
    ...["--network", network, "--network-alias", SIM_ALIAS],
    ...["-p", `127.0.0.1::${CONTROL_PORT}`],
    ...["--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges"],
    ...["-v", `${resolve(root, "web/test/moonraker-sim")}:/sim:ro`, "-v", `${keyDir}:/key:ro`],
    ...[image, "node", "/sim/moonraker-sim.mjs", "--api-key-file", "/key/api-key"],
    // Inside the container's own network namespace: its private interface and its loopback.
    ...["--host", "0.0.0.0", "--port", String(PRINTER_PORT)],
    ...["--control-host", "0.0.0.0", "--control-port", String(CONTROL_PORT)],
  ];
}

/**
 * The host addresses a container's ports are published on that are not host loopback,
 * from `docker inspect --format '{{json .NetworkSettings.Ports}}'`. An empty list means
 * every published port is on 127.0.0.1 or ::1. An empty HostIp means all interfaces.
 */
export function publishedOffLoopback(ports) {
  return Object.entries(ports ?? {}).flatMap(([port, bindings]) =>
    (bindings ?? [])
      .filter(({ HostIp }) => HostIp !== "127.0.0.1" && HostIp !== "::1")
      .map(({ HostIp, HostPort }) => `${port} on ${HostIp || "all interfaces"}:${HostPort}`),
  );
}

/** True for an RFC 1918 IPv4 address, which pp-net's outbound rule accepts as a printer. */
export function isPrivateIPv4(address) {
  const parts = address.split(".").map(Number);
  if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) {
    return false;
  }
  const [a, b] = parts;
  return a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168);
}

/** The status code and body of a raw HTTP/1.x answer. */
export function parseAnswer(text) {
  const status = /^HTTP\/1\.[01] (\d{3})/.exec(text);
  const split = text.indexOf("\r\n\r\n");
  return { status: status ? Number(status[1]) : null, body: split === -1 ? "" : text.slice(split + 4) };
}
