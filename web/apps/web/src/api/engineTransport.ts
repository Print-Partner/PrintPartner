import {
  notifyEngineUnauthorized,
  resolveEngineUrl,
} from "./contractRequest";

export class EngineHttpError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly body: unknown = null,
  ) {
    super(message);
    this.name = "EngineHttpError";
  }
}

type EngineRequest = Readonly<{
  path: string;
  method?: string;
  headers?: HeadersInit;
  body?: BodyInit | null;
  signal?: AbortSignal;
  failureMessage?: string;
}>;

type EngineMultipartRequest = Omit<EngineRequest, "body"> &
  Readonly<{
    form: FormData;
  }>;

function errorDetail(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  if ("detail" in value && typeof value.detail === "string" && value.detail.trim()) {
    return value.detail;
  }
  // The Build rename rule's 409 carries the taken name instead of a detail.
  if ("code" in value && value.code === "duplicate_build_name" && "name" in value
    && typeof value.name === "string") {
    return `Profile already exists: ${value.name}`;
  }
  return undefined;
}

function isCurrentPasswordRejection(value: unknown): boolean {
  if (typeof value !== "object" || value === null || !("code" in value)) return false;
  return value.code === "current_password_rejected";
}

async function failedResponseBody(response: Response): Promise<unknown> {
  try {
    const value: unknown = await response.json();
    return value;
  } catch {
    return null;
  }
}

async function engineResponse(input: EngineRequest): Promise<Response> {
  const response = await fetch(resolveEngineUrl(input.path), {
    method: input.method,
    headers: input.headers,
    body: input.body,
    signal: input.signal,
    credentials: "include",
  });
  if (!response.ok) {
    const body = await failedResponseBody(response);
    if (response.status === 401 && !isCurrentPasswordRejection(body)) {
      notifyEngineUnauthorized();
    }
    const fallback = input.failureMessage ?? `Engine ${input.path} failed`;
    throw new EngineHttpError(
      errorDetail(body) ?? `${fallback}: ${response.status}`,
      response.status,
      body,
    );
  }
  return response;
}

export function randomIdempotencyKey(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `idem-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`;
}

export async function engineFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  const isFormData = typeof FormData !== "undefined" && init?.body instanceof FormData;
  if (init?.body && !isFormData && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const res = await engineResponse({
    path,
    method: init?.method,
    headers,
    body: init?.body,
    signal: init?.signal ?? undefined,
  });
  if (res.status === 204) {
    return undefined as T;
  }
  const contentType = res.headers.get("content-type") ?? "";
  if (!contentType.includes("application/json")) {
    const snippet = (await res.text()).trimStart().slice(0, 40);
    if (snippet.startsWith("<!") || snippet.toLowerCase().startsWith("<html")) {
      throw new Error(
        `Engine ${path} returned HTML instead of JSON — check API route and dev proxy`,
      );
    }
    throw new Error(`Engine ${path} expected JSON but got ${contentType || "unknown type"}`);
  }
  return res.json() as Promise<T>;
}

export async function engineFetchMultipart<T>(
  input: EngineMultipartRequest,
): Promise<T> {
  const response = await engineResponse({
    path: input.path,
    method: input.method ?? "POST",
    headers: input.headers,
    body: input.form,
    signal: input.signal,
    failureMessage: input.failureMessage,
  });
  return response.json();
}

export async function engineSendMultipart(
  input: EngineMultipartRequest,
): Promise<void> {
  await engineResponse({
    path: input.path,
    method: input.method ?? "POST",
    headers: input.headers,
    body: input.form,
    signal: input.signal,
    failureMessage: input.failureMessage,
  });
}

export async function engineFetchStream(input: EngineRequest): Promise<Response> {
  return engineResponse(input);
}

export async function engineFetchText(path: string): Promise<string> {
  const res = await engineResponse({ path });
  return res.text();
}
