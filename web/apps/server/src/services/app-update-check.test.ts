import { afterEach, describe, expect, it, vi } from "vitest";
import {
  checkAppUpdate,
  compareAppVersions,
  isUpdateAvailable,
  normalizeAppVersion,
  resetAppUpdateCheckCache,
} from "./app-update-check.js";
import type { UpdateCheckConfig } from "./app-update-check.js";

const baseConfig: UpdateCheckConfig = {
  version: "0.1.0-web",
  deployMode: "self-host",
  updateCheckEnabled: true,
  githubRepo: "Print-Partner/PrintPartner",
  latestVersionOverride: null,
  updateCheckCacheHours: 12,
};

describe("app-update-check version utils", () => {
  it("normalizes health and GitHub tag formats", () => {
    expect(normalizeAppVersion("0.1.0-web")).toBe("0.1.0");
    expect(normalizeAppVersion("v0.2.0")).toBe("0.2.0");
    expect(normalizeAppVersion("V1.0.0")).toBe("1.0.0");
  });

  it("compares versions with semver-style ordering", () => {
    expect(compareAppVersions("0.1.0-web", "0.2.0")).toBe(-1);
    expect(compareAppVersions("0.2.0", "0.1.0-web")).toBe(1);
    expect(compareAppVersions("1.0.0", "1.0.0")).toBe(0);
    expect(isUpdateAvailable("0.1.0-web", "0.2.0")).toBe(true);
    expect(isUpdateAvailable("0.2.0", "0.1.0")).toBe(false);
  });
});

describe("checkAppUpdate", () => {
  afterEach(() => {
    resetAppUpdateCheckCache();
    vi.restoreAllMocks();
  });

  it("returns disabled payload when update check is off", async () => {
    const result = await checkAppUpdate({
      ...baseConfig,
      updateCheckEnabled: false,
    });
    expect(result.enabled).toBe(false);
    expect(result.update_available).toBe(false);
    expect(result.latest_version).toBeNull();
  });

  it("uses PRINT_PARTNER_LATEST_VERSION override without calling GitHub", async () => {
    const fetchImpl = vi.fn();
    const result = await checkAppUpdate(
      {
        ...baseConfig,
        latestVersionOverride: "0.3.0",
      },
      { fetchImpl },
    );
    expect(fetchImpl).not.toHaveBeenCalled();
    expect(result.update_available).toBe(true);
    expect(result.latest_version).toBe("0.3.0");
    expect(result.release_notes_url).toContain("github.com/Print-Partner/PrintPartner/releases");
  });

  it("reports update when GitHub latest is newer", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({
        tag_name: "v0.2.0",
        html_url: "https://github.com/Print-Partner/PrintPartner/releases/tag/v0.2.0",
      }), { status: 200 }),
    );
    const result = await checkAppUpdate(baseConfig, { fetchImpl });
    expect(result.update_available).toBe(true);
    expect(result.latest_version).toBe("v0.2.0");
    expect(result.release_url).toContain("/releases/tag/v0.2.0");
    expect(result.checked_at).toBeTruthy();
  });

  it("gracefully omits update when GitHub fetch fails", async () => {
    const fetchImpl = vi.fn().mockRejectedValue(new Error("network down"));
    const result = await checkAppUpdate(baseConfig, { fetchImpl });
    expect(result.update_available).toBe(false);
    expect(result.latest_version).toBeNull();
  });

  it("caches successful checks within TTL", async () => {
    let now = 1_000_000;
    const fetchImpl = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({ tag_name: "0.5.0", html_url: "https://example.com/r" }),
        { status: 200 },
      ),
    );
    await checkAppUpdate(baseConfig, { fetchImpl, now: () => now });
    now += 60_000;
    await checkAppUpdate(baseConfig, { fetchImpl, now: () => now });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });
});
