import { afterEach, describe, expect, it } from "vitest";
import { adoptStyleNonce } from "./csp";

const global = globalThis as { __webpack_nonce__?: string };

afterEach(() => {
  delete global.__webpack_nonce__;
  document.head.replaceChildren();
});

describe("adoptStyleNonce", () => {
  it("hands the gateway's nonce to get-nonce", () => {
    document.head.innerHTML = '<meta name="pp-style-nonce" nonce="abc123">';
    expect(adoptStyleNonce()).toBe("abc123");
    expect(global.__webpack_nonce__).toBe("abc123");
  });

  it("sets nothing when the document has no nonce element", () => {
    expect(adoptStyleNonce()).toBeUndefined();
    expect(global.__webpack_nonce__).toBeUndefined();
  });

  it("ignores an empty nonce", () => {
    document.head.innerHTML = '<meta name="pp-style-nonce" nonce="">';
    expect(adoptStyleNonce()).toBeUndefined();
    expect(global.__webpack_nonce__).toBeUndefined();
  });
});
