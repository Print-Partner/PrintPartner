import { chmodSync } from "node:fs";
import {
  installDesktopContext,
  desktopContext,
  mappedDesktopTenant,
  mappedDesktopDatabaseAvailable,
  closeDesktopAccount,
  releaseOrphanedDesktopOwner,
} from "./desktop-context.js";
import { interruptPrinterUploadsOnShutdown } from "./services/printer-send-queue.js";

async function setupFromParent(): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let buffered = "";
    const timer = setTimeout(() => reject(new Error("Desktop setup timed out")), 5000);
    const read = (chunk: Buffer) => {
      buffered += chunk.toString("utf8");
      if (buffered.length > 8192) {
        clearTimeout(timer);
        reject(new Error("Desktop setup rejected"));
        return;
      }
      const end = buffered.indexOf("\n");
      if (end < 0) return;
      process.stdin.off("data", read);
      clearTimeout(timer);
      try {
        resolve(JSON.parse(buffered.slice(0, end)));
      } catch {
        reject(new Error("Desktop setup rejected"));
      }
    };
    process.stdin.on("data", read);
    process.stdin.once("end", () => {
      clearTimeout(timer);
      reject(new Error("Desktop parent disconnected"));
    });
  });
}

// Unix kills this process's whole group. On Windows the core's Job Object
// terminates any process left behind once this one exits.
const exitDesktop = () =>
  process.platform === "win32" ? process.exit(0) : process.kill(-process.pid, "SIGKILL");

let stopping = false;
let runtime: Awaited<ReturnType<typeof import("./app.js").startServer>> | null = null;
const stop = async () => {
  if (stopping) return;
  stopping = true;
  const deadline = setTimeout(exitDesktop, 9000);
  try {
    const setup = desktopContext();
    if (runtime && setup) {
      const tenant = mappedDesktopTenant();
      interruptPrinterUploadsOnShutdown(
        setup.data_dir,
        tenant && !mappedDesktopDatabaseAvailable() ? {} : runtime.ports,
        tenant ?? undefined,
      );
    }
    await runtime?.app.close();
    await runtime?.ports.db.close();
    closeDesktopAccount();
  } finally {
    clearTimeout(deadline);
    const setup = desktopContext();
    if (setup && process.ppid !== setup.parent_pid) releaseOrphanedDesktopOwner(setup);
    exitDesktop();
  }
};

try {
  installDesktopContext(await setupFromParent());
  process.stdin.once("end", () => {
    void stop();
  });
  process.once("SIGTERM", () => {
    void stop();
  });
  process.once("SIGINT", () => {
    void stop();
  });
  const setup = desktopContext();
  if (!setup) throw new Error("Desktop setup missing");
  process.env.PRINT_PARTNER_DATA_DIR = setup.data_dir;
  process.env.PRINT_PARTNER_UPDATE_CHECK = "0";
  const { loadConfig } = await import("./config.js");
  const { startServer } = await import("./app.js");
  const config = loadConfig();
  config.port = setup.port;
  config.authRequired = true;
  config.registrationOpen = false;
  config.multiUser = mappedDesktopTenant() !== null;
  config.singleUserAuth = false;
  config.trustProxy = false;
  config.staticDir = null;
  config.databaseUrl = null;
  config.deployMode = "self-host";
  runtime = await startServer(config);
  // A Windows named pipe has the DACL the core gave it.
  if (process.platform !== "win32") chmodSync(setup.socket_path, 0o600);
  if (stopping) await stop();
} catch {
  process.stderr.write('{"event":"desktop_start_failed"}\n');
  await stop();
}
