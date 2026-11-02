import multipart from "@fastify/multipart";
import Fastify from "fastify";
import { zipSync } from "fflate";
import { describe, expect, it, vi } from "vitest";
import { KIT_JSON_TOO_LARGE_DETAIL, MAX_KIT_JSON_BYTES } from "../services/export-kit.js";
import { registerImportRoutes } from "./imports.js";

function oversizedDeclaredKitArchive(): Buffer {
  const payload = Buffer.from('{"format":"print-partner-kit","version":3,"layers":[],"parts":[]}');
  const archive = Buffer.from(zipSync({ "kit.json": payload }, { level: 9 }));
  const localHeader = archive.indexOf(Buffer.from([0x50, 0x4b, 0x03, 0x04]));
  const centralHeader = archive.lastIndexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02]));
  if (localHeader < 0 || centralHeader < 0) {
    throw new Error("ZIP fixture is missing its entry headers");
  }
  archive.writeUInt32LE(MAX_KIT_JSON_BYTES + 1, localHeader + 22);
  archive.writeUInt32LE(MAX_KIT_JSON_BYTES + 1, centralHeader + 24);
  return archive;
}

function multipartKitUpload(
  content: Buffer,
  fieldName = "file",
): {
  headers: Record<string, string>;
  payload: Buffer;
} {
  const boundary = "----pp-kit-boundary";
  return {
    headers: {
      "content-type": `multipart/form-data; boundary=${boundary}`,
    },
    payload: Buffer.concat([
      Buffer.from(
        `--${boundary}\r\n` +
          `Content-Disposition: form-data; name="${fieldName}"; filename="hostile.print-partner-kit.zip"\r\n` +
          "Content-Type: application/zip\r\n\r\n",
      ),
      content,
      Buffer.from(`\r\n--${boundary}--\r\n`),
    ]),
  };
}

describe("kit bundle import route", () => {
  it("maps an oversized expanded kit.json to a public 413 response", async () => {
    const app = Fastify();
    await app.register(multipart);
    const importKitBundle = vi.fn(() => ({
      profile_id: 1,
      profile_name: "Imported",
      parts_imported: 0,
      layers_imported: 0,
      warnings: [],
      unmatched_sources: [],
    }));
    await registerImportRoutes(app, { repo: { importKitBundle } });

    try {
      const response = await app.inject({
        method: "POST",
        url: "/imports/kit-bundle",
        ...multipartKitUpload(oversizedDeclaredKitArchive()),
      });

      expect(response.statusCode).toBe(413);
      expect(response.json()).toEqual({
        detail: KIT_JSON_TOO_LARGE_DETAIL,
      });
      expect(importKitBundle).not.toHaveBeenCalled();
    } finally {
      await app.close();
    }
  });

  it("drains an unexpected file field before returning a 400 response", async () => {
    const app = Fastify();
    await app.register(multipart);
    const importKitBundle = vi.fn();
    await registerImportRoutes(app, { repo: { importKitBundle } });

    const abortController = new AbortController();
    const timeout = setTimeout(() => abortController.abort(), 1_000);

    try {
      const response = await app.inject({
        method: "POST",
        url: "/imports/kit-bundle",
        ...multipartKitUpload(Buffer.from("not a kit"), "unexpected"),
        signal: abortController.signal,
      });

      expect(abortController.signal.aborted).toBe(false);
      expect(response.statusCode).toBe(400);
      expect(response.json()).toEqual({ detail: "Kit bundle file required" });
      expect(importKitBundle).not.toHaveBeenCalled();
    } finally {
      clearTimeout(timeout);
      await app.close();
    }
  });
});
