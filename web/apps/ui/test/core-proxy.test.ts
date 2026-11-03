// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { CORE_PATHS, coreProxy, originBridge } from "../core-proxy";

describe("dev server core proxy", () => {
  it("forwards nothing when PP_UI_CORE_URL is unset or blank", () => {
    expect(coreProxy({})).toEqual({});
    expect(coreProxy({ PP_UI_CORE_URL: "  " })).toEqual({});
  });

  it("ignores the legacy Node target variable", () => {
    expect(coreProxy({ VITE_DEV_API_TARGET: "http://127.0.0.1:18765" })).toEqual({});
  });

  it("sends every core path to the configured Rust core only", () => {
    const proxy = coreProxy({ PP_UI_CORE_URL: "http://127.0.0.1:8080" });
    expect(Object.keys(proxy)).toEqual([...CORE_PATHS]);
    for (const options of Object.values(proxy)) {
      expect(options.target).toBe("http://127.0.0.1:8080");
    }
    expect(proxy["/ws"].ws).toBe(true);
    expect(proxy["/api"].ws).toBe(false);
  });

  it.each([
    "127.0.0.1:8080",
    "ftp://127.0.0.1",
    "http://127.0.0.1:8080/api",
    "http://user:pw@127.0.0.1",
    "not a url",
  ])("rejects %j", (value) => {
    expect(() => coreProxy({ PP_UI_CORE_URL: value })).toThrow(
      /PP_UI_CORE_URL must be an http\(s\) origin/,
    );
  });

  it("presents the core's origin for loopback browsers and leaves other origins alone", () => {
    const listeners: Record<string, (proxyReq: unknown, req: unknown) => void> = {};
    originBridge("http://127.0.0.1:8080")({
      on: (event, listener) => {
        listeners[event] = listener as (proxyReq: unknown, req: unknown) => void;
      },
    });
    expect(Object.keys(listeners).sort()).toEqual(["proxyReq", "proxyReqWs"]);
    const forward = (origin: string | undefined) => {
      const set: Record<string, string> = {};
      for (const listener of Object.values(listeners)) {
        listener({ setHeader: (name: string, value: string) => (set[name] = value) }, { headers: { origin } });
      }
      return set.origin;
    };
    expect(forward("http://127.0.0.1:5174")).toBe("http://127.0.0.1:8080");
    expect(forward("http://localhost:5174")).toBe("http://127.0.0.1:8080");
    expect(forward("http://evil.example")).toBeUndefined();
    expect(forward("http://127.0.0.1.evil.example")).toBeUndefined();
    expect(forward(undefined)).toBeUndefined();
  });

  it("wires the origin bridge into every proxied path", () => {
    const proxy = coreProxy({ PP_UI_CORE_URL: "http://127.0.0.1:8080" });
    for (const options of Object.values(proxy)) expect(typeof options.configure).toBe("function");
  });

  it("has no Node API target in the Vite config", () => {
    const config = readFileSync(new URL("../vite.config.ts", import.meta.url), "utf8");
    expect(config).not.toMatch(/18765|VITE_DEV_API_TARGET|apps\/server/);
  });
});
