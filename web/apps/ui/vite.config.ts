import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
import { CORE_URL_ENV, coreProxy } from "./core-proxy.ts";

export default defineConfig(({ command, mode }) => {
  const proxy = coreProxy(process.env);
  if (command === "serve" && mode !== "test" && Object.keys(proxy).length === 0) {
    console.warn(
      `${CORE_URL_ENV} is not set; the dev server forwards no requests to the Rust core.`,
    );
  }
  return {
    plugins: [react()],
    server: { host: "127.0.0.1", port: 5174, strictPort: true, proxy },
    preview: { host: "127.0.0.1", port: 4174, strictPort: true, proxy },
    build: { target: "es2022", outDir: "dist" },
    test: {
      environment: "jsdom",
      include: ["src/**/*.test.{ts,tsx}", "test/**/*.test.ts"],
    },
  };
});
