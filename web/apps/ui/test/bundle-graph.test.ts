// @vitest-environment node
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { build, type InlineConfig, type Plugin } from "vite";
import { describe, expect, it } from "vitest";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const FIX = "Load three with import() behind a WebGL check (GTK-04)";

const THREE_MODULE_ID = /(?:^|[\\/]node_modules[\\/])(?:three|@react-three[\\/][^\\/]+)(?:[\\/]|$)/;

type Modules = ReadonlyMap<string, { staticImports: readonly string[]; entry: boolean }>;

async function parseGraph(config: InlineConfig): Promise<Modules> {
  const modules = new Map<string, { staticImports: readonly string[]; entry: boolean }>();
  const recorder: Plugin = {
    name: "record-import-graph",
    moduleParsed(info) {
      modules.set(info.id, { staticImports: info.importedIds, entry: info.isEntry });
    },
  };
  await build({
    ...config,
    root: UI_ROOT,
    logLevel: "silent",
    plugins: [...(config.plugins ?? []), recorder],
    build: { ...config.build, write: false, minify: false },
  });
  return modules;
}

function reachFrom(modules: Modules, entry: string) {
  const reach = new Map<string, readonly string[]>([[entry, [entry]]]);
  const queue = [entry];
  for (const id of queue) {
    for (const next of modules.get(id)?.staticImports ?? []) {
      if (reach.has(next)) continue;
      reach.set(next, [...reach.get(id)!, next]);
      queue.push(next);
    }
  }
  return reach;
}

const short = (id: string) =>
  path.relative(UI_ROOT, id.replace("\0", "")).replaceAll("\\", "/") || id;

const endsAtFirstThreeModule = (chain: readonly string[]) =>
  chain.findIndex((id) => THREE_MODULE_ID.test(id)) === chain.length - 1;

const offenders = (reach: ReadonlyMap<string, readonly string[]>) =>
  [...reach.values()].filter(endsAtFirstThreeModule).map((chain) => chain.map(short).join(" -> "));

const FIXTURES: Record<string, string> = {
  "\0fx:static": 'import "fx:hop";',
  "\0fx:hop": 'import { REVISION } from "three"; export const r = REVISION;',
  "\0fx:dynamic": 'export const load = () => import("three");',
};
const fixtures: Plugin = {
  name: "fixtures",
  enforce: "pre",
  resolveId: (id) => (id.startsWith("fx:") ? `\0${id}` : null),
  load: (id) => FIXTURES[id] ?? null,
};

describe("GTK-04 Three.js is not in the initial import graph", { timeout: 120_000 }, () => {
  it.each([
    ["three", true],
    ["three/examples/jsm/controls/OrbitControls.js", true],
    ["@react-three/fiber", true],
    ["/w/node_modules/three/build/three.module.js", true],
    ["/w/node_modules/.pnpm/three@0.186.1/node_modules/three/build/three.core.js", true],
    ["C:\\w\\node_modules\\@react-three\\drei\\index.js", true],
    ["three-stdlib", false],
    ["/w/node_modules/three-stdlib/index.js", false],
    ["/w/apps/ui/src/three/scene.tsx", false],
  ])("treats %s as WebGL: %s", (id, webgl) => {
    expect(THREE_MODULE_ID.test(id)).toBe(webgl);
  });

  it("finds the chain when a module the entry imports statically imports three", async () => {
    const modules = await parseGraph({
      configFile: false,
      plugins: [fixtures],
      build: { rolldownOptions: { input: { a: "fx:static", b: "fx:dynamic" } } },
    });
    expect(offenders(reachFrom(modules, "\0fx:static"))).toEqual([
      "fx:static -> fx:hop -> ../../node_modules/three/build/three.module.js",
    ]);
    expect(offenders(reachFrom(modules, "\0fx:dynamic")), "import() is not a static edge").toEqual(
      [],
    );
    expect(
      [...modules.keys()].some((id) => THREE_MODULE_ID.test(id)),
      "three was parsed, so the dynamic edge was seen and told apart",
    ).toBe(true);
  });

  it("the app's index.html reaches no WebGL module through static imports", async () => {
    const env = process.env.NODE_ENV;
    process.env.NODE_ENV = "production";
    let modules: Modules;
    try {
      modules = await parseGraph({});
    } finally {
      process.env.NODE_ENV = env;
    }
    const entries = [...modules].filter(([, m]) => m.entry).map(([id]) => id);
    expect(entries.map(short)).toEqual(["index.html"]);
    const reach = reachFrom(modules, entries[0]!);
    const reached = [...reach.keys()].map(short);
    expect(reached, "the walk covered the real app").toEqual(
      expect.arrayContaining(["src/main.tsx", "src/builds/PlatePreview.tsx"]),
    );
    const found = offenders(reach);
    expect(found, `${FIX}. Static chains:\n${found.join("\n")}`).toEqual([]);
  });
});
