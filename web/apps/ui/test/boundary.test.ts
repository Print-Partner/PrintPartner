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
    [
      "export const lazy = () => import(`@print-partner/contracts`);",
      "template literal dynamic import",
    ],
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

  describe("layers inside src (GRE-1537)", () => {
    const planted = (code: string) => `${code}\nexport {};\n`;

    it.each([
      [
        'import { dateText } from "../builds/format";',
        "apps/ui/src/today/planted.ts",
        "screen to screen",
      ],
      [
        'import { beds } from "../today/beds";',
        "apps/ui/src/frame/planted.ts",
        "shared layer to screen",
      ],
      [
        'import { routes } from "../routes";',
        "apps/ui/src/farm/planted.ts",
        "screen to composition",
      ],
      [
        'import { App } from "../App";',
        "apps/ui/src/core/planted.ts",
        "shared layer to composition",
      ],
      [
        'export { dateText } from "../builds/format";',
        "apps/ui/src/library/planted.ts",
        "re-export",
      ],
      [
        'import { dateText } from "../builds/format";',
        "apps/ui/src/farm/planted.test.tsx",
        "screen to screen in a test",
      ],
      ['import * as builds from "../builds";', "apps/ui/src/today/planted.ts", "a screen folder"],
      [
        'import * as builds from "../builds";',
        "apps/ui/src/today/planted.test.ts",
        "a screen folder in a test",
      ],
      [
        'export const load = () => import("../farm/FarmLog");',
        "apps/ui/src/library/planted.ts",
        "dynamic import",
      ],
    ])("fails a planted import: %s in %s (%s)", async (code, file) => {
      expect(await boundaryErrors(planted(code), file)).toHaveLength(1);
    });

    it.each([
      [
        'import { queryScreenState } from "../screen/queryState";',
        "apps/ui/src/today/planted.ts",
        "screen to shared layer",
      ],
      [
        'import { fetchCore } from "../core/client";',
        "apps/ui/src/frame/planted.ts",
        "shared layer to shared layer",
      ],
      [
        'import { dateText } from "./format";',
        "apps/ui/src/builds/planted.ts",
        "inside one screen",
      ],
      [
        'import { BuildWorkspace } from "./builds/BuildWorkspace";',
        "apps/ui/src/planted.ts",
        "composition to a screen",
      ],
      [
        'import { App } from "../App";',
        "apps/ui/src/farm/planted.test.tsx",
        "a test renders the app",
      ],
      [
        'import { routes } from "../routes";',
        "apps/ui/src/builds/planted.test-utils.tsx",
        "test utilities render the app",
      ],
      ['import * as here from ".";', "apps/ui/src/today/planted.ts", "its own folder"],
      [
        'import { dateText } from "../src/builds/format";',
        "apps/ui/test/planted.ts",
        "a file outside src",
      ],
    ])("allows a planted import: %s in %s (%s)", async (code, file) => {
      expect(await boundaryErrors(planted(code), file)).toEqual([]);
    });
  });

  it("leaves the legacy app's imports alone", async () => {
    const code = 'import { x } from "@print-partner/contracts";\nexport { x };\n';
    expect(await boundaryErrors(code, "apps/web/src/planted.ts")).toEqual([]);
  });
});
