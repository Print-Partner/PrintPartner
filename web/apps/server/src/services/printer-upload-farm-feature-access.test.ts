import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { tenantStorage } from "../middleware/tenant-context.js";
import { moonrakerAdapter } from "../integrations/adapters/moonraker.js";
import { prusalinkAdapter } from "../integrations/adapters/prusalink.js";
import { createJobRunner } from "./job-runner.js";
import {
  enqueuePrinterSend,
  getPrinterSendQueueItem,
  updatePrinterSendQueueItem,
} from "./printer-send-queue-store.js";

const disabled = "FEATURE_DISABLED: Feature farm is disabled.";
const invalid = "FEATURE_TOGGLES_INVALID: Stored feature toggles are invalid.";
const bytes = "G1 X1 Y1\n";

async function fixture(type: "moonraker" | "prusalink" = "moonraker") {
  const root = mkdtempSync(join(tmpdir(), "pp-upload-farm-worker-"));
  const ports = createSelfHostPorts(root);
  await ports.db.connect();
  const repo = ports.repository;
  const jobs = createJobRunner(() => repo, root);
  const profileId = repo.createProfile("Farm Worker Plan").id;
  function host(hostType = type) {
    repo.setSetting(
      "integrations",
      JSON.stringify([{ id: "host", type: hostType, name: "Workshop", config: {} }]),
    );
    repo.setSetting("printer.fleet", JSON.stringify([{ id: "printer", integration_id: "host" }]));
  }
  host();
  const upload = vi
    .spyOn(moonrakerAdapter, "uploadFile")
    .mockResolvedValue({ ok: true, started: false });
  const prusaUpload = vi
    .spyOn(prusalinkAdapter, "uploadFile")
    .mockResolvedValue({ ok: true, started: false });
  const fetch = vi.fn(() => Promise.reject(new Error("Unexpected network request")));
  vi.stubGlobal("fetch", fetch);
  let serial = 0;
  async function run(
    options: {
      payload?: Record<string, unknown>;
      queued?: boolean;
      tenant?: string;
      beforeRun?: () => void;
    } = {},
  ) {
    const tenant = options.tenant ?? "default";
    const path = join(
      jobs.getExportsDir(tenant),
      "printer-uploads",
      String(++serial),
      "plate.gcode",
    );
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, bytes);
    const payload = {
      printer_id: "printer",
      artifact_path: path,
      filename: "plate.gcode",
      start: false,
      profile_id: profileId,
      ...options.payload,
    };
    const item = options.queued
      ? tenantStorage.run(tenant, () =>
          enqueuePrinterSend(repo, {
            printer_id: "printer",
            artifact_path: path,
            filename: "plate.gcode",
            start: false,
            profile_id: profileId,
          }),
        )
      : null;
    const id = await jobs.start("printer-upload", payload, tenant);
    if (item)
      tenantStorage.run(tenant, () =>
        updatePrinterSendQueueItem(repo, item.id, { state: "sending", upload_job_id: id }),
      );
    options.beforeRun?.();
    const terminal = await jobs.waitForTerminal(id, 3000, tenant);
    await new Promise<void>((resolve) => setImmediate(resolve));
    expect(fetch).not.toHaveBeenCalled();
    const adapter = type === "moonraker" ? upload : prusaUpload;
    const stored = item
      ? tenantStorage.run(tenant, () => getPrinterSendQueueItem(repo, item.id))
      : null;
    if (item) {
      expect(stored).toMatchObject({
        id: item.id,
        state: terminal.status === "done" ? "done" : "error",
        artifact_path: path,
        profile_id: profileId,
        upload_job_id: id,
      });
      if (terminal.status === "error") expect(readFileSync(path, "utf8")).toBe(bytes);
      else expect(existsSync(path)).toBe(false);
    } else if (terminal.status === "error") {
      expect(existsSync(path)).toBe(false);
    } else {
      expect(existsSync(path)).toBe(false);
    }
    return { ...terminal, queueItem: stored, adapter };
  }
  return {
    repo,
    jobs,
    profileId,
    host,
    upload,
    prusaUpload,
    run,
    close: async () => {
      await ports.db.close();
      rmSync(root, { recursive: true, force: true });
    },
  };
}

describe.each(["moonraker", "prusalink"] as const)(
  "printer-upload worker farm admission (%s)",
  (hostType) => {
    let f: Awaited<ReturnType<typeof fixture>>;
    beforeEach(async () => {
      vi.stubEnv("PP_DESKTOP_REBUILD", "1");
      vi.stubEnv("DEPLOY_MODE", "self-host");
      f = await fixture(hostType);
    });
    afterEach(async () => {
      await f.close();
      vi.restoreAllMocks();
      vi.unstubAllEnvs();
      vi.unstubAllGlobals();
    });

    it.each([
      ['{"farm":false}', disabled],
      ['{"farm":null}', invalid],
    ])("reconciles the exact queue error for stored choices %s", async (stored, error) => {
      f.repo.setSetting("feature_toggles", stored);
      const result = await f.run({ queued: true });
      expect(result.queueItem).toMatchObject({ state: "error", error });
      expect(result).toMatchObject({ status: "error", error });
      expect(result.adapter).not.toHaveBeenCalled();
    });

    for (const queued of [false, true]) {
      it.each([undefined, "{}", '{"farm":false}', '{"bambu":true,"orders":true,"spoolman":true}'])(
        `denies default/explicit farm off %j with queued=${queued}`,
        async (stored) => {
          if (stored !== undefined) f.repo.setSetting("feature_toggles", stored);
          expect(await f.run({ queued })).toMatchObject({
            status: "error",
            error: disabled,
            result: null,
          });
          expect(f.upload).not.toHaveBeenCalled();
          expect(f.prusaUpload).not.toHaveBeenCalled();
        },
      );
      it.each([
        "",
        "{",
        "[]",
        "null",
        '{"farm":null}',
        '{"farm":"true"}',
        '{"farm":true,"orders":0}',
      ])(`fails closed on stored settings %j with queued=${queued}`, async (stored) => {
        for (const key of ["feature_toggles", `feature_toggles:plan:${f.profileId}`]) {
          f.repo.setSetting("feature_toggles", '{"farm":true}');
          f.repo.setSetting(`feature_toggles:plan:${f.profileId}`, "{}");
          f.repo.setSetting(key, stored);
          expect(await f.run({ queued })).toMatchObject({ status: "error", error: invalid });
          expect(f.upload).not.toHaveBeenCalled();
          expect(f.prusaUpload).not.toHaveBeenCalled();
        }
      });
      it(`reads live Plan precedence and revocation with queued=${queued}`, async () => {
        const key = `feature_toggles:plan:${f.profileId}`;
        f.repo.setSetting("feature_toggles", '{"farm":false}');
        f.repo.setSetting(key, '{"farm":true}');
        expect(await f.run({ queued })).toMatchObject({ status: "done", error: null });
        expect(
          await f.run({ queued, beforeRun: () => f.repo.setSetting(key, '{"farm":false}') }),
        ).toMatchObject({ error: disabled });
        f.repo.setSetting("feature_toggles", '{"farm":true}');
        expect(await f.run({ queued })).toMatchObject({ error: disabled });
        f.repo.setSetting(key, "{}");
        expect(await f.run({ queued })).toMatchObject({ status: "done", error: null });
        expect(
          await f.run({
            queued,
            beforeRun: () => f.repo.setSetting("feature_toggles", '{"farm":false}'),
          }),
        ).toMatchObject({ error: disabled });
      });
    }

    it.each([undefined, null, "", "broken", 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, {}, []])(
      "rejects missing/malformed stored Plan %j",
      async (profile_id) => {
        f.repo.setSetting("feature_toggles", '{"farm":true}');
        expect(await f.run({ payload: { profile_id } })).toMatchObject({
          status: "error",
          error: "Pick a plan to bind this send (profile_id required)",
        });
        expect(f.upload).not.toHaveBeenCalled();
        expect(f.prusaUpload).not.toHaveBeenCalled();
      },
    );

    it("rejects nonexistent and foreign Plans before feature reads", async () => {
      const foreign = tenantStorage.run("other", () => f.repo.createProfile("Foreign"));
      const reads = vi.spyOn(f.repo, "getSettingSnapshot");
      for (const profile_id of [999999, foreign.id]) {
        expect(await f.run({ payload: { profile_id } })).toMatchObject({
          error: "Profile not found",
        });
        expect(reads).not.toHaveBeenCalled();
      }
    });

    it("isolates tenant and unrelated Plan settings", async () => {
      f.repo.setSetting(`feature_toggles:plan:${f.repo.createProfile("Unrelated").id}`, "broken");
      f.repo.setSetting("feature_toggles", '{"farm":true}');
      const other = tenantStorage.run("other", () => {
        f.host();
        f.repo.setSetting("feature_toggles", '{"farm":false}');
        return f.repo.createProfile("Other");
      });
      expect(await f.run()).toMatchObject({ status: "done", error: null });
      expect(
        await f.run({ tenant: "other", payload: { profile_id: other.id, _tenant_id: "default" } }),
      ).toMatchObject({ error: disabled });
    });

    it.each(["printer_id", "profile_id"])(
      "rejects queued payload %s spoofing and preserves the artifact",
      async (key) => {
        const enabledPlan = f.repo.createProfile("Enabled");
        f.repo.setSetting(`feature_toggles:plan:${enabledPlan.id}`, '{"farm":true}');
        expect(
          await f.run({
            queued: true,
            payload: { [key]: key === "printer_id" ? "other-printer" : enabledPlan.id },
          }),
        ).toMatchObject({ error: "Stored printer upload does not match its queue item" });
      },
    );

    it("accepts an existing numeric string Plan from the job payload", async () => {
      f.repo.setSetting(`feature_toggles:plan:${f.profileId}`, '{"farm":true}');
      expect(await f.run({ payload: { profile_id: String(f.profileId) } })).toMatchObject({
        status: "done",
        error: null,
      });
    });

    it("fails closed when a queued Plan is removed before execution", async () => {
      f.repo.setSetting("feature_toggles", '{"farm":true}');
      expect(
        await f.run({ queued: true, beforeRun: () => f.repo.deleteProfile(f.profileId) }),
      ).toMatchObject({ error: "Profile not found" });
    });

    it.each(["1", "true"])("enforces exact flag %j", async (flag) => {
      vi.stubEnv("PP_DESKTOP_REBUILD", flag);
      expect(await f.run()).toMatchObject({ error: disabled });
      expect(f.upload).not.toHaveBeenCalled();
      expect(f.prusaUpload).not.toHaveBeenCalled();
    });

    it.each([undefined, "", "0", "false", "TRUE", "true ", "yes", "2"])(
      "preserves legacy transfer without feature reads for flag %j",
      async (flag) => {
        if (flag === undefined) delete process.env.PP_DESKTOP_REBUILD;
        else vi.stubEnv("PP_DESKTOP_REBUILD", flag);
        f.repo.setSetting("feature_toggles", "broken");
        const reads = vi.spyOn(f.repo, "getSettingSnapshot");
        expect(await f.run({ payload: { profile_id: undefined }, queued: true })).toMatchObject({
          status: "done",
          error: null,
        });
        expect(reads).not.toHaveBeenCalled();
        expect(hostType === "moonraker" ? f.upload : f.prusaUpload).toHaveBeenCalled();
      },
    );
  },
);
