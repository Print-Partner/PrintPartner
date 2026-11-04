import { useQuery } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { checkCoreHealth } from "../core/client";

export type ConnectionStatus = "checking" | "online" | "offline";

/** Why mutations are off while offline. Shown as text and used as the accessible description. */
export const OFFLINE_REASON =
  "Offline: Print Partner cannot reach its core, so changes are off until it reconnects.";

const ConnectionContext = createContext<ConnectionStatus>("checking");
const SimulateContext = createContext<(status: ConnectionStatus | null) => void>(() => {});

const ONLINE_POLL_MS = 30_000;
const OFFLINE_POLL_MS = 5_000;
/** A core that accepts the connection but never answers counts as offline after this. */
export const HEALTH_TIMEOUT_MS = 4_000;

/**
 * Polls GET /health and shares the result with every screen. The core is the only
 * judge of "online": a page that loaded but cannot reach the core is offline.
 */
export function ConnectionProvider({ children }: { children: ReactNode }) {
  const [simulated, setSimulated] = useState<ConnectionStatus | null>(null);
  const health = useQuery({
    queryKey: ["core", "health"],
    queryFn: async () => {
      await checkCoreHealth(AbortSignal.timeout(HEALTH_TIMEOUT_MS));
      return true;
    },
    // The default "online" mode pauses the probe while the browser reports no network,
    // which would leave the last success standing. The probe itself decides.
    networkMode: "always",
    retry: false,
    refetchInterval: (query) => (query.state.status === "error" ? OFFLINE_POLL_MS : ONLINE_POLL_MS),
  });
  const probed: ConnectionStatus = health.isError
    ? "offline"
    : health.isSuccess
      ? "online"
      : "checking";
  return (
    <SimulateContext value={setSimulated}>
      <ConnectionContext value={simulated ?? probed}>{children}</ConnectionContext>
    </SimulateContext>
  );
}

/**
 * Makes the whole frame, header included, act as if the core were in `status` while the
 * calling screen is mounted; null leaves the probe in charge. For the `/states` specimen,
 * so its offline sample cannot sit under a header that says the core is reachable.
 */
export function useSimulatedConnection(status: ConnectionStatus | null) {
  const simulate = useContext(SimulateContext);
  useEffect(() => {
    simulate(status);
    return () => simulate(null);
  }, [simulate, status]);
}

export const useConnection = () => useContext(ConnectionContext);

/** A test seam: render children as if the core were in this state. */
export function ConnectionOverride({
  status,
  children,
}: {
  status: ConnectionStatus;
  children: ReactNode;
}) {
  return <ConnectionContext value={status}>{children}</ConnectionContext>;
}
