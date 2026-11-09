// Run-time settings the Rust-only gateway's Content-Security-Policy needs. See
// docs/network-sharing.md#content-security-policy.
import { z } from "zod";

// The policy allows a <style> element only when it carries that response's nonce. Radix's
// scroll lock (react-style-singleton, through get-nonce) inserts one while a dialog is
// open and reads the nonce from the `__webpack_nonce__` global. The gateway puts the
// nonce on <meta name="pp-style-nonce"> at the top of <head>.
//
// Browsers that hide nonces return "" from getAttribute and the value from `.nonce`.
// With no element (the Vite dev and preview servers send no CSP), nothing is set.
export function adoptStyleNonce(doc: Document = document): string | undefined {
  const element = doc.querySelector<HTMLMetaElement>('meta[name="pp-style-nonce"]');
  const nonce = element?.nonce || element?.getAttribute("nonce") || undefined;
  if (nonce) (globalThis as { __webpack_nonce__?: string }).__webpack_nonce__ = nonce;
  return nonce;
}

// The policy has no 'unsafe-eval'. Zod probes `new Function` once to choose its JIT
// parser, and that caught probe still reports a violation. Jitless mode skips the probe
// and parses the same way. Schemas are built while modules load, so main.tsx imports
// this module first and the setting runs at its top level.
z.config({ jitless: true });
