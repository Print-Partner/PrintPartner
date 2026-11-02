import { basename } from "node:path";
import type { FastifyInstance } from "fastify";
import { openExportFileStream } from "../lib/secure-path.js";

type RouteDeps = { dataDir: string };

export async function registerExportRoutes(app: FastifyInstance, deps: RouteDeps): Promise<void> {
  app.get("/exports/*", async (request, reply) => {
    const wildcard = (request.params as { "*": string })["*"] ?? "";
    const key = wildcard.replace(/^\/+/, "");
    if (!key || key.includes("..")) {
      return reply.status(400).send({ detail: "Invalid export path" });
    }
    const stream = openExportFileStream(deps.dataDir, request.tenantId, key);
    if (!stream) {
      return reply.status(404).send({ detail: "Export file not found" });
    }
    const name = basename(key);
    const lower = name.toLowerCase();
    const isZip = lower.endsWith(".zip");
    const isHtml = lower.endsWith(".html");
    const is3mf = lower.endsWith(".3mf");
    const isPng = lower.endsWith(".png");
    const type = isZip
      ? "application/zip"
      : isHtml
        ? "text/html; charset=utf-8"
        : is3mf
          ? "model/3mf"
          : isPng
            ? "image/png"
            : "application/octet-stream";
    return reply
      .header("Content-Type", type)
      .header("X-Content-Type-Options", "nosniff")
      .header(
        "Content-Disposition",
        `${isPng ? "inline" : "attachment"}; filename="${name.replace(/[^\x20-\x7e]|["\\]/g, "_")}"; filename*=UTF-8''${encodeURIComponent(name).replace(/['()*]/g, (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`)}`,
      )
      .send(stream);
  });
}
