import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import { tenantExportDirectory } from "../lib/secure-path.js";
import { writeAcceptedExportFile } from "./accepted-export-publication.js";

const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture(): Readonly<{
  dataDir: string;
  reposDir: string;
  exportsDir: string;
}> {
  const dataDir = mkdtempSync(join(tmpdir(), "pp-accepted-export-quota-"));
  roots.push(dataDir);
  return {
    dataDir,
    reposDir: join(dataDir, "repos"),
    exportsDir: tenantExportDirectory(join(dataDir, "exports"), "tenant-a"),
  };
}

describe("writeAcceptedExportFile quota", () => {
  it("serializes same-tenant exports and rejects the second before its temporary write", async () => {
    const paths = fixture();
    const operation = (filename: string) =>
      runWithTenantDiskQuota(
        {
          dataDir: paths.dataDir,
          reposDir: paths.reposDir,
          tenantId: "tenant-a",
          sourceIds: () => [],
          quotaBytes: 10,
        },
        async () =>
          writeAcceptedExportFile({
            root: paths.exportsDir,
            directorySegments: ["direct"],
            filename,
            bytes: Buffer.from("123456"),
          }),
      );

    const [first, second] = await Promise.allSettled([
      operation("first.3mf"),
      operation("second.3mf"),
    ]);

    expect(first.status).toBe("fulfilled");
    expect(second.status).toBe("rejected");
    if (second.status === "rejected") {
      expect(second.reason).toBeInstanceOf(TenantDiskQuotaError);
    }
    expect(existsSync(join(paths.exportsDir, "direct", "first.3mf"))).toBe(true);
    expect(existsSync(join(paths.exportsDir, "direct", "second.3mf"))).toBe(false);
  });

  it("keeps self-host writes unlimited", async () => {
    const paths = fixture();
    const output = await runWithTenantDiskQuota(
      {
        dataDir: paths.dataDir,
        reposDir: paths.reposDir,
        tenantId: "default",
        sourceIds: () => [],
        quotaBytes: null,
      },
      async () =>
        writeAcceptedExportFile({
          root: paths.exportsDir,
          directorySegments: ["direct"],
          filename: "large.3mf",
          bytes: Buffer.alloc(128),
        }),
    );

    expect(existsSync(output)).toBe(true);
  });
});
