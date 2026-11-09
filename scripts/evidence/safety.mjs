// Error handling for capture.mjs that never prints a secret. Playwright's errors carry a
// call log with every request header, including the session cookie, and navigation
// errors carry the URL, which for the bootstrap link is a sign-in secret. So only
// CaptureError messages, written here from fixed text, reach the output.

/** An error whose message is safe to print: method, path, status or fixed text only. */
export class CaptureError extends Error {}

/** Awaits promise; on failure throws CaptureError(message) without the original or its cause. */
export async function guarded(promise, message) {
  try {
    return await promise;
  } catch {
    throw new CaptureError(message);
  }
}

/** A JSON client for the core on Playwright's APIRequestContext that redacts every failure. */
export function makeApi(request, origin) {
  return async (method, path, body, headers = {}) => {
    const response = await guarded(
      request.fetch(new URL(path, origin).href, {
        method,
        headers: { Origin: origin, ...(body === undefined ? {} : { "Content-Type": "application/json" }), ...headers },
        data: body === undefined ? undefined : JSON.stringify(body),
        maxRedirects: 0,
      }),
      `${method} ${path}: transport error`,
    );
    const text = await guarded(response.text(), `${method} ${path}: reading the body failed`);
    if (!response.ok()) throw new CaptureError(`${method} ${path} answered ${response.status()}${problemTitle(text)}`);
    try {
      return text ? JSON.parse(text) : null;
    } catch {
      throw new CaptureError(`${method} ${path}: the body is not JSON`);
    }
  };
}

// The core answers errors as RFC 9457 problems; their title is fixed server text.
function problemTitle(text) {
  try {
    const { title } = JSON.parse(text);
    return typeof title === "string" ? `: ${title.slice(0, 120)}` : "";
  } catch {
    return "";
  }
}

/**
 * Prints a failure and exits 1. A CaptureError prints its message; anything else prints
 * only its class name, because its message or stack can hold a cookie or the bootstrap link.
 */
export function report(error) {
  const message =
    error instanceof CaptureError
      ? error.message
      : `failed with ${error?.name ?? "an error"}; details withheld because they can hold the session cookie or bootstrap link`;
  process.stderr.write(`capture: ${message}\n`);
  process.exit(1);
}

/** Routes every uncaught error, including a rejected top-level await, through report. */
export function installReport() {
  process.on("uncaughtException", report);
  process.on("unhandledRejection", report);
}
