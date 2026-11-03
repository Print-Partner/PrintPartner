import eslint from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";
import { uiBoundary } from "./eslint-rules/ui-boundary.js";

export default tseslint.config(
  eslint.configs.recommended,
  ...tseslint.configs.recommended,
  {
    ignores: ["**/dist/**", "**/node_modules/**", "**/coverage/**"],
  },
  {
    files: ["**/*.{ts,tsx}"],
    plugins: {
      "react-hooks": reactHooks,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-hooks/set-state-in-effect": "off",
      "react-hooks/refs": "off",
      "react-hooks/exhaustive-deps": "warn",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      "@typescript-eslint/no-empty-object-type": "off",
      // Code names are camelCase, types PascalCase, constants UPPER_CASE. Property
      // keys and destructured names follow the JSON and SQLite fields they mirror.
      "@typescript-eslint/naming-convention": [
        "error",
        { selector: "default", format: ["camelCase"], leadingUnderscore: "allow" },
        // Renamed React component props, such as { icon: Icon }, are PascalCase.
        { selector: "parameter", format: ["camelCase", "PascalCase"], leadingUnderscore: "allow" },
        {
          selector: "variable",
          format: ["camelCase", "UPPER_CASE", "PascalCase"],
          leadingUnderscore: "allow",
        },
        { selector: ["variable", "parameter"], modifiers: ["destructured"], format: null },
        { selector: "function", format: ["camelCase", "PascalCase"] },
        { selector: "typeLike", format: ["PascalCase"] },
        { selector: "enumMember", format: ["PascalCase", "UPPER_CASE"] },
        { selector: "import", format: null },
        {
          selector: [
            "objectLiteralProperty",
            "objectLiteralMethod",
            "typeProperty",
            "typeMethod",
            "classProperty",
          ],
          format: null,
        },
      ],
    },
  },
  {
    // Existing violations live in eslint-suppressions.json. Lower those counts; never raise them.
    files: ["**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}"],
    rules: {
      complexity: ["error", { max: 20 }],
      "max-depth": ["error", { max: 5 }],
      "max-params": ["error", { max: 5 }],
    },
  },
  {
    // Generated from Rust contracts; fix names in the generator, not here.
    files: ["**/*.generated.ts"],
    rules: { "@typescript-eslint/naming-convention": "off" },
  },
  {
    // The replacement UI reaches the Rust core only; see apps/ui/README.md.
    files: ["apps/ui/**/*.{ts,tsx,mts,cts,js,jsx,mjs,cjs}"],
    plugins: { "print-partner": { rules: { "ui-boundary": uiBoundary } } },
    rules: { "print-partner/ui-boundary": "error" },
  },
  {
    files: ["apps/web/public/sw.js"],
    languageOptions: {
      globals: {
        self: "readonly",
        caches: "readonly",
        fetch: "readonly",
        Response: "readonly",
        URL: "readonly",
      },
    },
  },
);
