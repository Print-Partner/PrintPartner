// Keeps the replacement UI (apps/ui) off the legacy runtime: it may not import the
// old screens, the Node server, the hand-maintained contracts or the legacy domain
// package, by package name or by relative path.
//
// Inside apps/ui/src it also keeps three layers apart (GRE-1537):
// - a screen folder may not import another screen folder;
// - a shared layer may not import a screen folder;
// - a file outside composition (the files directly in src) may not import composition.
//   The `compositionImports: "allow"` option turns off this third check only; the config
//   sets it for test files, which render the composed app.
// A shared helper two screens need goes in a shared layer, never in one of the screens.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../apps/ui");
const WORKSPACE_SCOPE = "@print-partner/";
// Workspace packages apps/ui may import: only the client generated from Rust.
const ALLOWED_WORKSPACE_PACKAGES = new Set(["@print-partner/rust-client"]);
const SRC_ROOT = path.join(UI_ROOT, "src");
// Shared layers any screen may use. Any other folder directly under src is a screen,
// so a new folder is held to the screen rules until it is listed here.
const SHARED_LAYERS = new Set(["core", "frame", "screen", "status", "theme", "type", "fonts"]);

function isFolder(file) {
  try {
    return fs.statSync(file).isDirectory();
  } catch {
    return false;
  }
}

/** Where a file under src sits: composition, a shared layer or a screen; null outside src. */
function layerOf(file) {
  const relative = path.relative(SRC_ROOT, file);
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative)) return null;
  const [top, ...rest] = relative.split(path.sep);
  // A file directly in src is composition; a folder directly in src ("../builds") is that folder.
  if (rest.length === 0 && !isFolder(file)) return { kind: "composition", name: "src" };
  return { kind: SHARED_LAYERS.has(top) ? "shared" : "screen", name: top };
}

function layerViolation(specifier, filename, allowComposition) {
  if (!specifier.startsWith(".")) return null;
  const from = layerOf(filename);
  const to = layerOf(path.resolve(path.dirname(filename), specifier));
  if (!from || !to || from.name === to.name) return null;
  if (to.kind === "screen" && from.kind === "screen") {
    return `${specifier} imports the ${to.name} screen from the ${from.name} screen; a screen may not import another screen. Move what both need to a shared layer.`;
  }
  if (to.kind === "screen" && from.kind === "shared") {
    return `${specifier} imports the ${to.name} screen from the shared ${from.name} layer; a shared layer may not import a screen.`;
  }
  if (to.kind === "composition" && from.kind !== "composition" && !allowComposition) {
    return `${specifier} imports composition (the files directly in src) from ${from.name}; only composition wires the screens together.`;
  }
  return null;
}

function violation(specifier, filename) {
  if (specifier.startsWith(WORKSPACE_SCOPE)) {
    const name = specifier.split("/", 2).join("/");
    if (ALLOWED_WORKSPACE_PACKAGES.has(name)) return null;
    return `${name} is not on the apps/ui workspace allowlist; the replacement UI talks only to the Rust core.`;
  }
  if (specifier.startsWith(".") || path.isAbsolute(specifier)) {
    const relative = path.relative(UI_ROOT, path.resolve(path.dirname(filename), specifier));
    if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
      return `${specifier} leaves web/apps/ui; the replacement UI may not import the old app, the Node server or shared legacy packages.`;
    }
  }
  return null;
}

export const uiBoundary = {
  meta: {
    type: "problem",
    docs: {
      description:
        "Restrict apps/ui imports to its own files and allowed packages, and keep its layers apart",
    },
    schema: [
      {
        type: "object",
        properties: { compositionImports: { enum: ["refuse", "allow"] } },
        additionalProperties: false,
      },
    ],
  },
  create(context) {
    const allowComposition = context.options[0]?.compositionImports === "allow";
    function check(source) {
      let specifier;
      if (source?.type === "Literal" && typeof source.value === "string") specifier = source.value;
      // import(`@scope/pkg`) with no ${} expressions is as static as a quoted string.
      else if (source?.type === "TemplateLiteral" && source.expressions.length === 0) {
        specifier = source.quasis[0].value.cooked;
      }
      if (typeof specifier !== "string") return;
      const message =
        violation(specifier, context.filename) ??
        layerViolation(specifier, context.filename, allowComposition);
      if (message) context.report({ node: source, message });
    }
    return {
      ImportDeclaration: (node) => check(node.source),
      ExportNamedDeclaration: (node) => check(node.source),
      ExportAllDeclaration: (node) => check(node.source),
      ImportExpression: (node) => check(node.source),
      TSExternalModuleReference: (node) => check(node.expression),
      // typeof import("x") / import("x").T: the specifier sits in a literal type
      // (typescript-eslint 8) or directly on the node (newer parsers).
      TSImportType: (node) => check(node.source ?? node.argument?.literal ?? node.argument),
    };
  },
};
