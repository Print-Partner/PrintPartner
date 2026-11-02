import { describe, expect, it } from "vitest";
import {
  defaultDisplayName,
  isValidEmailInput,
  normalizeEmailInput,
  parseDiscordOAuthProfile,
  parseGitHubOAuthProfile,
  parseOAuthAccessToken,
  readOAuthError,
  selectVerifiedGitHubEmail,
} from "./auth-route-model.js";

describe("auth route model", () => {
  it("normalizes email-like input", () => {
    expect(normalizeEmailInput("  USER@Example.COM ")).toBe("user@example.com");
    expect(normalizeEmailInput(null)).toBe("");
  });

  it("validates basic email presence", () => {
    expect(isValidEmailInput("user@example.com")).toBe(true);
    expect(isValidEmailInput("user")).toBe(false);
    expect(isValidEmailInput("")).toBe(false);
  });

  it("derives display names", () => {
    expect(defaultDisplayName({ displayName: "  Pat  ", email: "user@example.com" })).toBe("Pat");
    expect(defaultDisplayName({ displayName: "", email: "user@example.com" })).toBe("user");
    expect(defaultDisplayName({ displayName: "", email: "" })).toBe("User");
  });

  it("selects a verified GitHub email and prefers the primary address", () => {
    expect(
      selectVerifiedGitHubEmail([
        { email: "secondary@example.com", verified: true, primary: false },
        { email: "primary@example.com", verified: true, primary: true },
      ]),
    ).toBe("primary@example.com");
    expect(
      selectVerifiedGitHubEmail([
        { email: "unverified@example.com", verified: false, primary: true },
        { email: "verified@example.com", verified: true, primary: false },
      ]),
    ).toBe("verified@example.com");
    expect(
      selectVerifiedGitHubEmail([
        { email: "unverified@example.com", verified: false, primary: true },
      ]),
    ).toBeNull();
    expect(selectVerifiedGitHubEmail({ email: "wrong-shape@example.com" })).toBeNull();
  });

  it("accepts only a non-empty OAuth access token and error", () => {
    expect(parseOAuthAccessToken({ access_token: " token " })).toBe("token");
    expect(parseOAuthAccessToken({ access_token: "" })).toBeNull();
    expect(parseOAuthAccessToken({ access_token: 42 })).toBeNull();
    expect(parseOAuthAccessToken(null)).toBeNull();
    expect(readOAuthError({ error: " access_denied " })).toBe("access_denied");
    expect(readOAuthError({ error: { message: "not trusted" } })).toBeNull();
  });

  it("requires a stable GitHub identity", () => {
    expect(parseGitHubOAuthProfile({ id: 42, login: " octocat ", name: " Octo Cat " })).toEqual({
      providerUserId: "42",
      login: "octocat",
      displayName: "Octo Cat",
    });
    expect(parseGitHubOAuthProfile({ login: "octocat" })).toBeNull();
    expect(parseGitHubOAuthProfile({ id: 42 })).toBeNull();
    expect(parseGitHubOAuthProfile({ id: -1, login: "octocat" })).toBeNull();
  });

  it("uses a Discord email only when Discord marks it verified", () => {
    expect(
      parseDiscordOAuthProfile({
        id: "123",
        username: " printer ",
        global_name: " Print Friend ",
        email: " USER@example.com ",
        verified: true,
      }),
    ).toEqual({
      providerUserId: "123",
      login: "printer",
      displayName: "Print Friend",
      email: "user@example.com",
    });
    expect(
      parseDiscordOAuthProfile({
        id: "123",
        username: "printer",
        email: "victim@example.com",
        verified: false,
      }),
    ).toMatchObject({ email: null });
    expect(parseDiscordOAuthProfile({ username: "printer" })).toBeNull();
    expect(parseDiscordOAuthProfile({ id: "123" })).toBeNull();
  });
});
