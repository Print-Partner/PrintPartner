import { once } from "node:events";
import { createHmac } from "node:crypto";
import { createServer as createHttpServer } from "node:http";
import { createServer as createNetServer, type Socket } from "node:net";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import Fastify from "fastify";
import { describe, expect, it, vi } from "vitest";
import {
  acquireDataDirectory,
  dialDesktopPipe,
  releaseOrphanedDesktopOwner,
  serveDesktopPipe,
  verifyDesktopPrincipal,
} from "./desktop-context.js";
import { processIdentity } from "./storage-lease.js";

const setup = { key: "ab".repeat(32), generation: "cd".repeat(16) };
const principal = {
  version: 1,
  generation: setup.generation,
  tenant: "default",
  actor: "desktop-owner",
  method: "POST",
  target: "/plans/1/save?expected=2",
  issued_at: 100_000,
};
function signed(value: unknown) {
  const encoded = Buffer.from(JSON.stringify(value)).toString("hex");
  return {
    encoded,
    signature: createHmac("sha256", Buffer.from(setup.key, "hex")).update(encoded).digest("hex"),
    method: principal.method,
    target: principal.target,
    now: 100_500,
  };
}

describe("Windows private pipe dial-in", () => {
  it("retries until the core listens and serves one request per connection", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-dial-"));
    const path =
      process.platform === "win32"
        ? String.raw`\\.\pipe\pp-dial-test-${process.pid}`
        : join(directory, "core.sock");
    const http = createHttpServer((request, response) => response.end(`served ${request.url}`));
    const stop = dialDesktopPipe(http, path, 2);
    await new Promise((resolve) => setTimeout(resolve, 150));
    const accepted: Socket[] = [];
    const core = createNetServer((socket) => accepted.push(socket));
    core.listen(path);
    await once(core, "listening");
    const request = async (target: string) => {
      while (accepted.length === 0) await new Promise((resolve) => setTimeout(resolve, 10));
      const socket = accepted.shift()!;
      let body = "";
      socket.on("data", (chunk) => (body += chunk));
      socket.write(`GET ${target} HTTP/1.1\r\nHost: desktop-compat\r\nConnection: close\r\n\r\n`);
      await once(socket, "close");
      return body;
    };
    try {
      expect(await request("/first")).toContain("served /first");
      expect(await request("/second")).toContain("served /second");
      expect(await request("/third")).toContain("served /third");
    } finally {
      void stop();
      for (const socket of accepted) socket.destroy();
      core.close();
      http.closeAllConnections();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("keeps a dial waiting while earlier connections hold streams open", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-dial-"));
    const path =
      process.platform === "win32"
        ? String.raw`\\.\pipe\pp-dial-hold-${process.pid}`
        : join(directory, "core.sock");
    const http = createHttpServer((request, response) => {
      if (request.url === "/health") response.end("healthy");
      else response.write("streaming");
    });
    const accepted: Socket[] = [];
    const core = createNetServer((socket) => accepted.push(socket));
    core.listen(path);
    await once(core, "listening");
    const stop = dialDesktopPipe(http, path, 2);
    const take = async () => {
      const deadline = Date.now() + 2000;
      while (accepted.length === 0) {
        if (Date.now() > deadline) throw new Error("No dial waiting");
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      return accepted.shift()!;
    };
    const send = (socket: Socket, target: string, expected: string) => {
      const received = new Promise<void>((resolve) => {
        let body = "";
        socket.on("data", (chunk) => {
          body += chunk;
          if (body.includes(expected)) resolve();
        });
      });
      socket.write(`GET ${target} HTTP/1.1\r\nHost: desktop-compat\r\n\r\n`);
      return received;
    };
    const held: Socket[] = [];
    try {
      for (let stream = 0; stream < 4; stream += 1) {
        const socket = await take();
        held.push(socket);
        await send(socket, "/stream", "streaming");
      }
      await send(await take(), "/health", "healthy");
    } finally {
      void stop();
      for (const socket of [...held, ...accepted]) socket.destroy();
      core.close();
      http.closeAllConnections();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("finishes a request in flight before close lets the database go", async () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-dial-"));
    const path =
      process.platform === "win32"
        ? String.raw`\\.\pipe\pp-dial-drain-${process.pid}`
        : join(directory, "core.sock");
    const accepted: Socket[] = [];
    const core = createNetServer((socket) => accepted.push(socket));
    core.listen(path);
    await once(core, "listening");
    const events: string[] = [];
    let release = () => {};
    let started = () => {};
    const handlerStarted = new Promise<void>((resolve) => (started = resolve));
    const app = Fastify();
    app.post("/plans/1/save", async () => {
      started();
      await new Promise<void>((resolve) => (release = resolve));
      events.push("handler finished");
      return { saved: true };
    });
    await serveDesktopPipe(app, path);
    try {
      await vi.waitFor(() => expect(accepted.length).toBeGreaterThan(0));
      const socket = accepted.shift()!;
      let body = "";
      socket.on("data", (chunk) => (body += chunk));
      socket.write(
        "POST /plans/1/save HTTP/1.1\r\nHost: desktop-compat\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
      );
      await handlerStarted;
      const databaseClosed = app
        .close()
        .then(() => events.push("app closed"))
        .then(() => events.push("database closed"));
      await new Promise((resolve) => setTimeout(resolve, 100));
      expect(events).toEqual([]);
      release();
      await databaseClosed;
      expect(events).toEqual(["handler finished", "app closed", "database closed"]);
      await vi.waitFor(() => expect(body).toContain('{"saved":true}'));
    } finally {
      for (const socket of accepted) socket.destroy();
      core.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
});

describe("Rust-written storage markers", () => {
  it.each(["dead PID", "reused PID"])("recovers a marker with a %s", (scenario) => {
    const directory = mkdtempSync(join(tmpdir(), "pp-rust-stale-owner-"));
    const dead = scenario === "dead PID" ? spawnSync(process.execPath, ["-e", ""]) : null;
    if (dead) expect(dead.status).toBe(0);
    const owner = {
      pid: dead?.pid ?? process.pid,
      process_identity: `${processIdentity(process.pid)}-prior`,
      instance: "ab".repeat(16),
    };
    const marker = JSON.stringify({
      pid: owner.pid,
      process_identity: owner.process_identity,
      child_pid: owner.pid,
      child_process_identity: owner.process_identity,
      lease_hash: "cd".repeat(32),
      runtime_dir: join(tmpdir(), "pp-prior-runtime"),
    });
    try {
      mkdirSync(join(directory, ".desktop-lease"));
      writeFileSync(join(directory, ".desktop-lease/owner.json"), JSON.stringify(owner));
      writeFileSync(join(directory, ".desktop-owner.json"), marker, { mode: 0o600 });
      const release = acquireDataDirectory(directory);
      try {
        expect(readFileSync(join(directory, ".desktop-lease/previous-marker.json"), "utf8")).toBe(
          marker,
        );
        expect(JSON.parse(readFileSync(join(directory, ".desktop-owner.json"), "utf8"))).toEqual({
          pid: process.pid,
          kind: "standalone",
          process_identity: processIdentity(process.pid),
          instance: expect.stringMatching(/^[a-f0-9]{32}$/),
        });
        expect(() => acquireDataDirectory(directory)).toThrow("already owned");
      } finally {
        release();
      }
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("recovers a dead core marker before child metadata was recorded", () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-rust-no-child-"));
    const dead = spawnSync(process.execPath, ["-e", ""]);
    expect(dead.status).toBe(0);
    const marker = JSON.stringify({
      storage_protocol_version: 1,
      pid: dead.pid,
      process_identity: "prior",
      lease_hash: "cd".repeat(32),
    });
    try {
      writeFileSync(join(directory, ".desktop-owner.json"), marker);
      const release = acquireDataDirectory(directory);
      try {
        expect(readFileSync(join(directory, ".desktop-lease/previous-marker.json"), "utf8")).toBe(
          marker,
        );
        expect(() => acquireDataDirectory(directory)).toThrow("already owned");
      } finally {
        release();
      }
      acquireDataDirectory(directory)();
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it.each([undefined, 0, 2, "1", null])(
    "refuses a childless core marker without protocol version 1 (%j)",
    (version) => {
      const directory = mkdtempSync(join(tmpdir(), "pp-rust-legacy-no-child-"));
      const dead = spawnSync(process.execPath, ["-e", ""]);
      expect(dead.status).toBe(0);
      const marker = JSON.stringify({
        pid: dead.pid,
        process_identity: "prior",
        lease_hash: "cd".repeat(32),
        storage_protocol_version: version,
      });
      const path = join(directory, ".desktop-owner.json");
      try {
        writeFileSync(path, marker);
        expect(() => acquireDataDirectory(directory)).toThrow("already owned");
        expect(readFileSync(path, "utf8")).toBe(marker);
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },
  );

  it.each([
    { child_pid: process.pid },
    { child_process_identity: "prior" },
    { child_pid: null, child_process_identity: null },
    { child_pid: 0, child_process_identity: "prior" },
    { child_pid: process.pid, child_process_identity: "" },
    { child_pid: process.pid, child_process_identity: processIdentity(process.pid) },
  ])("refuses a dead core marker with partial, invalid or live child info %j", (child) => {
    const directory = mkdtempSync(join(tmpdir(), "pp-rust-child-owner-"));
    const dead = spawnSync(process.execPath, ["-e", ""]);
    expect(dead.status).toBe(0);
    const marker = JSON.stringify({
      pid: dead.pid,
      process_identity: "prior",
      storage_protocol_version: 1,
      lease_hash: "cd".repeat(32),
      ...child,
    });
    const path = join(directory, ".desktop-owner.json");
    try {
      writeFileSync(path, marker);
      expect(() => acquireDataDirectory(directory)).toThrow("already owned");
      expect(readFileSync(path, "utf8")).toBe(marker);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it.each([true, false])(
    "preserves a live owner's marker (identity present: %s)",
    (withIdentity) => {
      const directory = mkdtempSync(join(tmpdir(), "pp-rust-live-owner-"));
      const path = join(directory, ".desktop-owner.json");
      const marker = JSON.stringify({
        pid: process.pid,
        ...(withIdentity ? { process_identity: processIdentity(process.pid) } : {}),
        lease_hash: "cd".repeat(32),
        runtime_dir: join(tmpdir(), "pp-live-runtime"),
      });
      try {
        // No lease directory: the marker itself must protect a live legacy owner.
        writeFileSync(path, marker, { mode: 0o600 });
        expect(() => acquireDataDirectory(directory)).toThrow("already owned");
        expect(readFileSync(path, "utf8")).toBe(marker);
      } finally {
        rmSync(directory, { recursive: true, force: true });
      }
    },
  );
});

describe("desktop principal boundary", () => {
  it("accepts the exact authenticated request context", () => {
    expect(verifyDesktopPrincipal(signed(principal), setup)).toEqual(principal);
  });
  it.each([
    { version: 2 },
    { generation: "ef".repeat(16) },
    { tenant: "other" },
    { actor: "other" },
    { method: "GET" },
    { target: "/plans/2/save?expected=2" },
    { target: "/plans/1/save?expected=3" },
    { issued_at: 1 },
    { issued_at: 200_000 },
  ])("rejects a signed assertion with changed context %j", (changed) => {
    expect(() => verifyDesktopPrincipal(signed({ ...principal, ...changed }), setup)).toThrow();
  });
  it("rejects missing, forged and previous-generation signatures", () => {
    expect(() =>
      verifyDesktopPrincipal({ ...signed(principal), encoded: undefined }, setup),
    ).toThrow();
    expect(() =>
      verifyDesktopPrincipal({ ...signed(principal), signature: "00".repeat(32) }, setup),
    ).toThrow();
    expect(() =>
      verifyDesktopPrincipal(signed(principal), { ...setup, key: "fe".repeat(32) }),
    ).toThrow();
  });
});

it("arbitrates standalone writers through an exclusive marker", async () => {
  const { mkdtempSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { acquireDataDirectory } = await import("./desktop-context.js");
  const directory = mkdtempSync(join(tmpdir(), "pp-owner-test-"));
  try {
    const release = acquireDataDirectory(directory);
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
    release();
    release();
    acquireDataDirectory(directory)();
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

it("recovers a crash-left standalone marker and starts a later writer", async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, readFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { acquireDataDirectory } = await import("./desktop-context.js");
  const directory = mkdtempSync(join(tmpdir(), "pp-crashed-owner-"));
  const child = spawn(
    process.execPath,
    [
      "--import",
      "tsx",
      "--input-type=module",
      "-e",
      `import { acquireDataDirectory } from ${JSON.stringify(new URL("./desktop-context.ts", import.meta.url).href)};
     acquireDataDirectory(${JSON.stringify(directory)});
     console.log("owned"); setInterval(() => {}, 1000);`,
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  try {
    await Promise.race([
      once(child.stdout, "data"),
      once(child, "exit").then(() => {
        throw new Error("Owner exited before acquisition");
      }),
    ]);
    expect(JSON.parse(readFileSync(join(directory, ".desktop-owner.json"), "utf8")).pid).toBe(
      child.pid,
    );
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
    const exited = once(child, "exit");
    child.kill("SIGKILL");
    await exited;
    expect(JSON.parse(readFileSync(join(directory, ".desktop-owner.json"), "utf8")).pid).toBe(
      child.pid,
    );
    const release = acquireDataDirectory(directory);
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
    release();
  } finally {
    if (child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit");
      child.kill("SIGKILL");
      await exited;
    }
    rmSync(directory, { recursive: true, force: true });
  }
});

it("preserves a live legacy standalone marker without an OS lock", async () => {
  const { mkdtempSync, writeFileSync, readFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { acquireDataDirectory } = await import("./desktop-context.js");
  const directory = mkdtempSync(join(tmpdir(), "pp-live-owner-"));
  const marker = join(directory, ".desktop-owner.json");
  const contents = JSON.stringify({ pid: process.pid, kind: "standalone" });
  try {
    writeFileSync(marker, contents);
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
    expect(readFileSync(marker, "utf8")).toBe(contents);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

it("serializes competing stale-marker recovery across processes", async () => {
  const { spawn } = await import("node:child_process");
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const directory = mkdtempSync(join(tmpdir(), "pp-owner-race-"));
  const dead = spawn(process.execPath, ["-e", ""], { stdio: "ignore" });
  await once(dead, "exit");
  writeFileSync(
    join(directory, ".desktop-owner.json"),
    JSON.stringify({ pid: dead.pid, kind: "standalone" }),
  );
  mkdirSync(join(directory, ".desktop-lease"));
  writeFileSync(
    join(directory, ".desktop-lease/owner.json"),
    JSON.stringify({
      pid: dead.pid,
      process_identity: "prior-boot:prior-start",
      instance: "cd".repeat(16),
    }),
  );
  const script = `import { acquireDataDirectory } from ${JSON.stringify(new URL("./desktop-context.ts", import.meta.url).href)};
    try { acquireDataDirectory(${JSON.stringify(directory)}); console.log("owned"); }
    catch { console.log("blocked"); }
    setInterval(() => {}, 1000);`;
  const children = [0, 1].map(() =>
    spawn(process.execPath, ["--import", "tsx", "--input-type=module", "-e", script], {
      stdio: ["ignore", "pipe", "pipe"],
    }),
  );
  try {
    const results = await Promise.all(
      children.map(async (child) => {
        const [output] = await Promise.race([
          once(child.stdout, "data"),
          once(child, "exit").then(() => {
            throw new Error("Contender exited before reporting");
          }),
        ]);
        return output.toString().trim();
      }),
    );
    expect(results.sort()).toEqual(["blocked", "owned"]);
  } finally {
    await Promise.all(
      children.map(async (child) => {
        if (child.exitCode === null && child.signalCode === null) {
          const exited = once(child, "exit");
          child.kill("SIGKILL");
          await exited;
        }
      }),
    );
    rmSync(directory, { recursive: true, force: true });
  }
});

it("recovers reused-PID markers and leases with a different process identity", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { acquireDataDirectory } = await import("./desktop-context.js");
  const { processIdentity } = await import("./storage-lease.js");
  const directory = mkdtempSync(join(tmpdir(), "pp-reused-pid-"));
  try {
    const owner = {
      pid: process.pid,
      kind: "standalone",
      process_identity: `${processIdentity(process.pid)}-prior`,
      instance: "ab".repeat(16),
    };
    mkdirSync(join(directory, ".desktop-lease"));
    writeFileSync(join(directory, ".desktop-lease/owner.json"), JSON.stringify(owner));
    writeFileSync(join(directory, ".desktop-owner.json"), JSON.stringify(owner));
    const release = acquireDataDirectory(directory);
    const current = JSON.parse(readFileSync(join(directory, ".desktop-owner.json"), "utf8"));
    expect(current.process_identity).toBe(processIdentity(process.pid));
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
    release();
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

it("keeps a live matching-identity marker owned", async () => {
  const { mkdtempSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { acquireDataDirectory } = await import("./desktop-context.js");
  const { processIdentity } = await import("./storage-lease.js");
  const directory = mkdtempSync(join(tmpdir(), "pp-matching-pid-"));
  try {
    writeFileSync(
      join(directory, ".desktop-owner.json"),
      JSON.stringify({
        pid: process.pid,
        kind: "standalone",
        process_identity: processIdentity(process.pid),
      }),
    );
    expect(() => acquireDataDirectory(directory)).toThrow("already owned");
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("orphaned desktop owner release", () => {
  it("removes the private socket without touching standalone ownership", () => {
    const directory = mkdtempSync(join(tmpdir(), "pp-orphaned-owner-"));
    const marker = join(directory, ".desktop-owner.json");
    const socket = join(directory, "run", "pp.sock");
    try {
      mkdirSync(join(directory, "run"));
      writeFileSync(socket, "");
      writeFileSync(marker, "standalone owner", { mode: 0o600 });
      releaseOrphanedDesktopOwner({ data_dir: directory, socket_path: socket, parent_pid: 4242 });
      expect(readFileSync(marker, "utf8")).toBe("standalone owner");
      expect({ socket: existsSync(socket), socketDir: existsSync(join(directory, "run")) }).toEqual(
        { socket: false, socketDir: false },
      );
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
});

it.skipIf(process.platform === "win32")(
  "requires the core flag and the inherited printpartner.lock inode",
  () => {
    const module = new URL("./desktop-context.ts", import.meta.url).href;
    const source = `
import { installDesktopContext, desktopContext } from ${JSON.stringify(module)};
try {
  installDesktopContext({ version: 1, data_dir: process.argv[1], socket_path: process.argv[1] + "/backend.sock",
    generation: "ab".repeat(16), key: "cd".repeat(32), parent_pid: process.ppid, port: 43123 });
  console.log(desktopContext()?.data_dir === process.argv[1] ? "admitted" : "wrong directory");
} catch (error) { console.log(error.message); }
`;
    const probe = `
import fcntl, os, subprocess, sys, tempfile
with tempfile.TemporaryDirectory() as directory:
    directory = os.path.realpath(directory)
    os.mkfifo(directory + "/.desktop-owner.json")
    for flag, name in [("1", "printpartner.lock"), ("0", "printpartner.lock"), ("1", "wrong.lock")]:
        with open(directory + "/printpartner.lock", "w+b") as lock, open(directory + "/" + name, "r+b" if name == "printpartner.lock" else "w+b") as inherited:
            fcntl.flock(lock, fcntl.LOCK_EX)
            os.dup2(inherited.fileno(), 198, inheritable=True)
            child = subprocess.run([sys.argv[1], "--import", "tsx", "--input-type=module", "-e", sys.argv[2], directory],
                                   env={**os.environ, "PP_STORAGE_CORE_LOCK": flag}, close_fds=False, capture_output=True, text=True, timeout=30)
            assert child.returncode == 0, child.stderr
            print(child.stdout.strip())
            os.close(198)
`;
    const result = spawnSync("python3", ["-c", probe, process.execPath, source], {
      encoding: "utf8",
      timeout: 120_000,
    });
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout.trim().split("\n")).toEqual([
      "admitted",
      "Core storage lock required",
      "Inherited storage lock rejected",
    ]);
  },
  120_000,
);
