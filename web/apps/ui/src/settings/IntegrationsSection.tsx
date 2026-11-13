import type { RouteResponse } from "@print-partner/rust-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useState, type ReactNode } from "react";
import { dateText } from "../builds/format";
import { readCore, writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

type Integration = RouteResponse<"listIntegrations">["integrations"][number];

const KIND_NAMES: Record<Integration["kind"], string> = {
  moonraker: "Moonraker",
  prusalink: "PrusaLink",
  spoolman: "Spoolman",
  bambu: "Bambu",
  home_assistant: "Home Assistant",
  ai_assistant: "AI assistant",
  slicer_sidecar: "Slicer sidecar",
};

const integrationsQuery = {
  queryKey: ["core", "integrations"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listIntegrations", {}, signal),
} as const;

const pinQuery = (id: string) =>
  ({
    queryKey: ["core", "integrations", id, "tls-pin"],
    queryFn: ({ signal }: { signal: AbortSignal }) => readCore("readTlsPin", { id }, signal),
  }) as const;

const why = (failure: unknown) => reason(failure instanceof Error ? failure : new Error(String(failure)));

/**
 * What the core keeps for each credential: only whether one is set. The list route never
 * answers a key or password, so none can be shown here.
 */
function credentialText(integration: Integration): string | null {
  const parts = [
    integration.username !== null && `User ${integration.username}`,
    integration.has_api_key && "API key saved",
    integration.has_password && "Password saved",
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(", ") : null;
}

/**
 * Asks before forgetting the pin. Focus starts on Cancel; only the confirm button sends the
 * reset, once. A failed reset is shown and not resent.
 */
function ResetPin({ integration, onClose, onReset }: { integration: Integration; onClose: () => void; onReset: () => void }) {
  const connection = useConnection();
  const offline = connection !== "online";
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const reasonId = useId();
  const reset = async () => {
    if (busy || offline) return;
    setBusy(true);
    setError("");
    try {
      await writeCore("resetTlsPin", { id: integration.id });
      onReset();
    } catch (failure) {
      setError(`The certificate was not reset: ${why(failure)}.`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog title={`Reset the certificate for ${integration.name}?`} onClose={onClose}>
      <p>
        Print Partner forgets the pinned certificate. The next Test connection trusts whatever
        certificate answers at this printer's address, so run it only while you are sure the
        printer on your network is this same device.
      </p>
      {offline && <p id={reasonId}>{OFFLINE_REASON}</p>}
      {busy && <p role="status">Resetting the certificate.</p>}
      {error && <p role="alert" className="pp-field-error">{error}</p>}
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button" onClick={onClose}>Cancel</button>
        <button
          type="button"
          className="pp-button pp-button-primary"
          aria-disabled={busy || offline || undefined}
          aria-describedby={offline ? reasonId : undefined}
          onClick={() => void reset()}
        >
          Reset certificate
        </button>
      </div>
    </Dialog>
  );
}

/** The certificate a Bambu Test connection pinned, and its deliberate reset. */
function BambuPin({ integration, offline, onMessage }: { integration: Integration; offline: boolean; onMessage: (text: string) => void }) {
  const client = useQueryClient();
  const pin = useQuery(pinQuery(integration.id));
  const [confirming, setConfirming] = useState(false);
  const close = () => {
    setConfirming(false);
    // A reset whose answer was lost may still have applied, so read the pin again.
    void client.invalidateQueries({ queryKey: pinQuery(integration.id).queryKey });
  };
  let text: string;
  if (pin.data?.pinned) {
    text = ["Certificate pinned", pin.data.sha256_prefix, dateText(pin.data.pinned_at ?? null)].filter(Boolean).join(" · ");
  } else if (pin.data) {
    text = "Not pinned. Test connection to pin.";
  } else if (pin.isError) {
    text = `Certificate state unavailable: ${why(pin.error)}.`;
  } else {
    text = "Checking the certificate.";
  }
  return (
    <span className="pp-fact-detail" data-tls-pin={integration.id}>
      {text}
      {pin.data?.pinned && (
        <button
          type="button"
          className="pp-button"
          aria-disabled={offline || undefined}
          onClick={() => !offline && setConfirming(true)}
        >
          Reset certificate
        </button>
      )}
      {confirming && (
        <ResetPin
          integration={integration}
          onClose={close}
          onReset={() => {
            onMessage(`Certificate reset for ${integration.name}. The next Test connection pins the certificate the printer shows.`);
            close();
          }}
        />
      )}
    </span>
  );
}

function Row({ integration, offline, onMessage }: { integration: Integration; offline: boolean; onMessage: (text: string) => void }) {
  const credentials = credentialText(integration);
  return (
    <div className="pp-fact" data-integration={integration.id} data-kind={integration.kind}>
      <dt>{integration.name}</dt>
      <dd>
        {KIND_NAMES[integration.kind]}, {integration.enabled ? "on" : "off"}
        {integration.base_url !== null && <span className="pp-fact-detail">{integration.base_url}</span>}
        {credentials && <span className="pp-fact-detail">{credentials}</span>}
        {integration.kind === "spoolman" && (
          <span className="pp-fact-detail">
            Spoolman alone owns filament counts. Stock it cannot read shows as unknown, never as zero.
          </span>
        )}
        {integration.kind === "bambu" && <BambuPin integration={integration} offline={offline} onMessage={onMessage} />}
      </dd>
    </div>
  );
}

/**
 * The saved integrations from `GET /api/v2/integrations`: kind, address, and only whether
 * each credential is set. Bambu rows add the certificate pin. Adding and editing
 * integrations follow in their own change.
 */
export function IntegrationsSection() {
  const connection = useConnection();
  const offline = connection !== "online";
  const integrations = useQuery(integrationsQuery);
  const [message, setMessage] = useState("");
  const list = integrations.data?.integrations;
  let body: ReactNode;
  if (!list) {
    body = integrations.isError ? null : <p role="status">Loading integrations.</p>;
  } else if (list.length === 0) {
    body = <p>No integrations yet.</p>;
  } else {
    body = (
      <dl className="pp-facts">
        {list.map((integration) => (
          <Row key={integration.id} integration={integration} offline={offline} onMessage={setMessage} />
        ))}
      </dl>
    );
  }
  return (
    <>
      <p>Saved keys and passwords are never shown here; the core says only whether one is set.</p>
      {connection === "offline" && <p>{OFFLINE_REASON}</p>}
      {message && <p role="status">{message}</p>}
      {integrations.isError && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>
            {list ? "Could not refresh the integrations, so this list may be out of date" : "Could not load the integrations"}:{" "}
            {why(integrations.error)}
          </p>
          <button type="button" className="pp-button" onClick={() => void integrations.refetch()}>
            Reload integrations
          </button>
        </div>
      )}
      {body}
    </>
  );
}
