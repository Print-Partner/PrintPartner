export type AuthIdentityProvider = "github" | "discord";

export type SessionUser = {
  user_id: string;
  tenant_id: string;
  login: string;
  display_name: string;
  email: string | null;
  provider: "github" | "discord" | "email" | "basic" | "anonymous" | "desktop";
  hasPassword?: boolean;
  is_admin: boolean;
};

type PublicUser = {
  user_id: string;
  login: string;
  display_name: string;
  email: string | null;
  provider: SessionUser["provider"];
  hasPassword: boolean;
  is_admin: boolean;
};

export function isSyntheticAnonymousSession(user: SessionUser | null | undefined): boolean {
  if (!user || user.provider !== "anonymous") return false;
  return (
    user.user_id === "local" || (user.user_id === "anonymous" && user.tenant_id === "anonymous")
  );
}

export function toPublicUser(user: SessionUser): PublicUser {
  return {
    user_id: user.user_id,
    login: user.login,
    display_name: user.display_name,
    email: user.email,
    provider: user.provider,
    hasPassword: user.hasPassword ?? false,
    is_admin: user.is_admin,
  };
}
