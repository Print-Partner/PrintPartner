export function normalizeEmailInput(value: unknown): string {
  return typeof value === "string" ? value.trim().toLowerCase() : "";
}

export function isValidEmailInput(email: string): boolean {
  return Boolean(email && email.includes("@"));
}

export function defaultDisplayName(input: { displayName: unknown; email: string }): string {
  const explicit = typeof input.displayName === "string" ? input.displayName.trim() : "";
  return explicit || input.email.split("@")[0] || "User";
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}

function trimmedString(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  return trimmed || null;
}

export function parseOAuthAccessToken(value: unknown): string | null {
  const record = asRecord(value);
  return record ? trimmedString(record.access_token) : null;
}

export function readOAuthError(value: unknown): string | null {
  const record = asRecord(value);
  return record ? trimmedString(record.error) : null;
}

type OAuthProfile = {
  providerUserId: string;
  login: string;
  displayName: string;
};

export function parseGitHubOAuthProfile(value: unknown): OAuthProfile | null {
  const record = asRecord(value);
  if (!record) return null;
  const login = trimmedString(record.login);
  const id = record.id;
  if (!login || typeof id !== "number" || !Number.isSafeInteger(id) || id <= 0) return null;
  return {
    providerUserId: String(id),
    login,
    displayName: trimmedString(record.name) ?? login,
  };
}

type DiscordOAuthProfile = OAuthProfile & {
  email: string | null;
};

export function parseDiscordOAuthProfile(value: unknown): DiscordOAuthProfile | null {
  const record = asRecord(value);
  if (!record) return null;
  const providerUserId = trimmedString(record.id);
  const login = trimmedString(record.username);
  if (!providerUserId || !/^\d+$/.test(providerUserId) || !login) return null;
  const candidateEmail = record.verified === true ? normalizeEmailInput(record.email) : "";
  return {
    providerUserId,
    login,
    displayName: trimmedString(record.global_name) ?? login,
    email: isValidEmailInput(candidateEmail) ? candidateEmail : null,
  };
}

type VerifiedGitHubEmail = {
  email: string;
  primary: boolean;
};

function parseVerifiedGitHubEmail(value: unknown): VerifiedGitHubEmail | null {
  const record = asRecord(value);
  if (!record) return null;
  const email = trimmedString(record.email);
  if (!email || record.verified !== true) return null;
  return {
    email: normalizeEmailInput(email),
    primary: record.primary === true,
  };
}

export function selectVerifiedGitHubEmail(value: unknown): string | null {
  if (!Array.isArray(value)) return null;
  const verifiedEmails = value
    .map(parseVerifiedGitHubEmail)
    .filter((email): email is VerifiedGitHubEmail => email !== null);
  return verifiedEmails.find((email) => email.primary)?.email ?? verifiedEmails[0]?.email ?? null;
}
