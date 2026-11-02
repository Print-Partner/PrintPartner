import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { writeAcceptedMediaPng } from "../lib/accepted-media-cache.js";
import { runWithTenantDiskQuota, TenantDiskQuotaError } from "../lib/tenant-disk-quota.js";
import { acceptedPartMediaIdentity } from "./accepted-part-media.js";
import type {
  AcceptedExportPart,
  CaptureAcceptedOperationalExportResult,
} from "./accepted-operational-export.js";
import { materializeAcceptedChecklistHtml } from "./export-html.js";

const roots: string[] = [];
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAF/gL+AvzZVwAAAABJRU5ErkJggg==",
  "base64",
);

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "print-partner-accepted-checklist-"));
  roots.push(root);
  return { tenantExportsDir: join(root, "exports"), thumbsDir: join(root, "thumbs") };
}

function acceptedPart(): AcceptedExportPart {
  return {
    revisionPartId: 31,
    projectionPartId: 41,
    partKey: "base:widget.stl",
    relativePath: "parts/widget.stl",
    filename: "<widget>.stl",
    sourceLayer: "base:Source & One",
    status: "ok",
    role: "accent",
    filamentColorId: null,
    filamentCustomHex: "#aabbcc",
    spoolmanSpoolId: null,
    quantityInferred: 2,
    quantityOverride: null,
    quantityEffective: 2,
    included: true,
    notes: "use <care>",
    geometrySame: null,
    requirement: null,
    optionGroupId: null,
    manifestSource: null,
    artifact: {
      kind: "tracked",
      sourceId: 1,
      sourceRevisionId: 2,
      snapshotRoot: "/accepted/source",
      relativePath: "parts/widget.stl",
      expectedSha256: "a".repeat(64),
    },
    units: [
      { token: "31:0", unitIndex: 0, completed: true, assembled: false },
      { token: "31:1", unitIndex: 1, completed: false, assembled: false },
    ],
  };
}

function capture(
  part: AcceptedExportPart,
  profileName = "Accepted <Build>",
  profileId = 7,
): Extract<CaptureAcceptedOperationalExportResult, { readonly kind: "ready" }> {
  return {
    kind: "ready",
    export: {
      basis: {
        profileId,
        planVersion: 4,
        revisionId: 19,
        revisionDigest: "b".repeat(64),
        requiredUnitMappingDigest: "c".repeat(64),
      },
      profile: {
        id: profileId,
        name: profileName,
        orderNumber: "SO&7",
        specialRequest: null,
        archivedAt: null,
      },
      provenance: { kind: "legacy" },
      parts: [part],
    },
  };
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("materializeAcceptedChecklistHtml", () => {
  it("preserves quota rejection from accepted export publication", async () => {
    const paths = fixture();

    await expect(
      runWithTenantDiskQuota(
        {
          dataDir: dirname(paths.tenantExportsDir),
          reposDir: join(dirname(paths.tenantExportsDir), "repos"),
          tenantId: "default",
          sourceIds: () => [],
          quotaBytes: 1,
        },
        async () =>
          materializeAcceptedChecklistHtml({
            capture: capture(acceptedPart()),
            ...paths,
            dateFormat: "ymd_24h",
            generatedAt: "2026-08-21T15:00:00.000Z",
          }),
      ),
    ).rejects.toBeInstanceOf(TenantDiskQuotaError);
  });

  it("renders accepted quantity, completion, escaping, and content-addressed thumbnail bytes", () => {
    const paths = fixture();
    const part = acceptedPart();
    const basis = acceptedPartMediaIdentity(
      {
        artifact: part.artifact,
        effectiveRole: part.role,
        filamentColorId: part.filamentColorId,
        filamentCustomHex: part.filamentCustomHex,
      },
      "thumbnail",
    ).basis;
    writeAcceptedMediaPng({ thumbsDir: paths.thumbsDir, basis, png: PNG });

    const result = materializeAcceptedChecklistHtml({
      capture: capture(part),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    const html = readFileSync(result.path, "utf8");
    expect(html).toContain("Accepted &lt;Build&gt;");
    expect(html).toContain("SO&amp;7");
    expect(html).toContain("&lt;widget&gt;.stl");
    expect(html).toContain("use &lt;care&gt;");
    expect(html).toContain("<td>2</td><td>—</td>");
    expect(html).toContain(`data:image/png;base64,${PNG.toString("base64")}`);
    expect(result.thumbCount).toBe(1);
    expect(result.basis).toEqual(capture(part).export.basis);
  });

  it("renders legacy accepted Parts without consulting a working thumbnail path", () => {
    const paths = fixture();
    const part = {
      ...acceptedPart(),
      artifact: { kind: "unavailable", reason: "legacy" },
    } satisfies AcceptedExportPart;

    const result = materializeAcceptedChecklistHtml({
      capture: capture(part),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(result.kind).toBe("materialized");
    if (result.kind !== "materialized") return;
    expect(result.thumbCount).toBe(0);
    expect(readFileSync(result.path, "utf8")).not.toContain("data:image/png");
  });

  it("rejects an intermediate export-directory symlink", () => {
    const paths = fixture();
    const outside = join(paths.tenantExportsDir, "..", "outside");
    mkdirSync(paths.tenantExportsDir, { recursive: true });
    mkdirSync(outside);
    symlinkSync(outside, join(paths.tenantExportsDir, "profile-7-Build"));

    const result = materializeAcceptedChecklistHtml({
      capture: capture(acceptedPart(), "Build"),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(result).toEqual({ kind: "output_failure" });
    expect(readdirSync(outside)).toEqual([]);
  });

  it("rejects a symlinked tenant export root", () => {
    const paths = fixture();
    const outside = join(paths.tenantExportsDir, "..", "outside-root");
    mkdirSync(outside);
    symlinkSync(outside, paths.tenantExportsDir);

    const result = materializeAcceptedChecklistHtml({
      capture: capture(acceptedPart(), "Build"),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(result).toEqual({ kind: "output_failure" });
    expect(readdirSync(outside)).toEqual([]);
  });

  it("retains prior accepted checklist bytes after a later snapshot publishes", () => {
    const paths = fixture();
    const first = materializeAcceptedChecklistHtml({
      capture: capture(acceptedPart(), "Build"),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });
    expect(first.kind).toBe("materialized");
    if (first.kind !== "materialized") return;
    const firstBytes = readFileSync(first.path);
    const changed = {
      ...acceptedPart(),
      units: acceptedPart().units.map((unit) => ({ ...unit, completed: true })),
    };
    const second = materializeAcceptedChecklistHtml({
      capture: capture(changed, "Build"),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(second.kind).toBe("materialized");
    if (second.kind !== "materialized") return;
    expect(second.path).not.toBe(first.path);
    expect(readFileSync(first.path)).toEqual(firstBytes);
    expect(readFileSync(second.path)).not.toEqual(firstBytes);
  });

  it("separates Builds whose readable names have the same slug", () => {
    const paths = fixture();
    const first = materializeAcceptedChecklistHtml({
      capture: capture(acceptedPart(), "A/B", 7),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });
    const second = materializeAcceptedChecklistHtml({
      capture: capture(acceptedPart(), "A_B", 8),
      ...paths,
      dateFormat: "ymd_24h",
      generatedAt: "2026-08-21T15:00:00.000Z",
    });

    expect(first.kind).toBe("materialized");
    expect(second.kind).toBe("materialized");
    if (first.kind !== "materialized" || second.kind !== "materialized") return;
    expect(first.path).not.toBe(second.path);
  });
});
