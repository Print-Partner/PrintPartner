// @vitest-environment node
import { ESLint } from "eslint";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const WEB_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const eslint = new ESLint({ cwd: WEB_ROOT });

async function boundaryErrors(code: string, file = "apps/ui/src/planted.ts") {
  const [result] = await eslint.lintText(code, { filePath: path.join(WEB_ROOT, file) });
  return result.messages.filter((message) => message.ruleId === "print-partner/ui-boundary");
}

// The first lint builds ESLint's config and parser for the whole package, which takes
// several seconds on a loaded machine; the default 5 s limit made the first case flaky.
describe("apps/ui import boundary", { timeout: 30_000 }, () => {
  it.each([
    ['import { x } from "@print-partner/contracts";', "contracts by name"],
    ['import { x } from "@print-partner/contracts/schemas";', "contracts subpath"],
    ['import type { X } from "@print-partner/domain";', "legacy domain"],
    ['import { x } from "@print-partner/server";', "Node server by name"],
    ['import { x } from "@print-partner/web";', "legacy app by name"],
    ['import { x } from "../../web/src/App";', "legacy app by path"],
    ['import { x } from "../../server/src/app";', "Node server by path"],
    ['import { x } from "../../../packages/contracts/src/index";', "contracts by path"],
    ['export * from "../../web/src/api/client";', "re-export"],
    ['export const load = () => import("../../server/src/app");', "dynamic import"],
    ['export type Legacy = typeof import("@print-partner/contracts");', "import type expression"],
    ["export const lazy = () => import(`@print-partner/contracts`);", "template literal dynamic import"],
  ])("fails a planted import: %s (%s)", async (code) => {
    expect(await boundaryErrors(`${code}\nexport {};\n`)).toHaveLength(1);
  });

  it("fails a planted import from a config file outside src", async () => {
    const errors = await boundaryErrors(
      'import config from "../web/vite.config";\nexport { config };\n',
      "apps/ui/vite.config.ts",
    );
    expect(errors).toHaveLength(1);
  });

  it("allows imports inside apps/ui, npm packages and the generated Rust client", async () => {
    const code = [
      'import { fetchCore } from "./core/client";',
      'import { routes } from "../src/routes";',
      'import { useQuery } from "@tanstack/react-query";',
      'import { printerDisplayDescriptors } from "@print-partner/rust-client";',
      "export { fetchCore, routes, useQuery, printerDisplayDescriptors };",
    ].join("\n");
    expect(await boundaryErrors(code)).toEqual([]);
  });

  it("leaves the legacy app's imports alone", async () => {
    const code = 'import { x } from "@print-partner/contracts";\nexport { x };\n';
    expect(await boundaryErrors(code, "apps/web/src/planted.ts")).toEqual([]);
  });
});
