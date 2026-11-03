// Keeps the replacement UI (apps/ui) off the legacy runtime: it may not import the
// old screens, the Node server, the hand-maintained contracts or the legacy domain
// package, by package name or by relative path.
import path from "node:path";
import { fileURLToPath } from "node:url";

const UI_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../apps/ui");
const WORKSPACE_SCOPE = "@print-partner/";
// Workspace packages apps/ui may import: only the client generated from Rust.
const ALLOWED_WORKSPACE_PACKAGES = new Set(["@print-partner/rust-client"]);

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
    docs: { description: "Restrict apps/ui imports to its own files and allowed packages" },
    schema: [],
  },
  create(context) {
    function check(source) {
      let specifier;
      if (source?.type === "Literal" && typeof source.value === "string") specifier = source.value;
      // import(`@scope/pkg`) with no ${} expressions is as static as a quoted string.
      else if (source?.type === "TemplateLiteral" && source.expressions.length === 0) {
        specifier = source.quasis[0].value.cooked;
      }
      if (typeof specifier !== "string") return;
      const message = violation(specifier, context.filename);
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
