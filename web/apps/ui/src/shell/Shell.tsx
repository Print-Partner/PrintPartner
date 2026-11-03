import { useQuery } from "@tanstack/react-query";
import { Outlet } from "react-router";
import { checkCoreHealth } from "../core/client";

function CoreStatus() {
  const health = useQuery({
    queryKey: ["core", "health"],
    queryFn: async () => {
      await checkCoreHealth();
      return true;
    },
    retry: false,
  });
  const label = health.isPending
    ? "Checking core"
    : health.isSuccess
      ? "Core reachable"
      : "Core unreachable";
  return (
    <p role="status" className="core-status">
      {label}
    </p>
  );
}

/** Placeholder app frame. Product screens arrive in their own slices. */
export function Shell() {
  return (
    <div className="shell">
      <header className="shell-header">
        <span className="shell-name">Print Partner</span>
        <CoreStatus />
      </header>
      <main className="shell-main">
        <Outlet />
      </main>
    </div>
  );
}

export function Placeholder() {
  return (
    <section>
      <h1>Replacement UI</h1>
      <p>No screens yet. This package builds against the Rust core only.</p>
    </section>
  );
}

export function NotFound() {
  return (
    <section>
      <h1>Not found</h1>
      <p>This address has no screen.</p>
    </section>
  );
}
