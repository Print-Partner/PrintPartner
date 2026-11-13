import type { RouteResponse } from "@print-partner/rust-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type ReactNode } from "react";
import { dateText } from "../builds/format";
import { CoreError } from "../core/client";
import { readCore, RequestContractError, writeCore } from "../core/rust";
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

/** A reset the core refused, or never sent: the pin is unchanged. */
const refused = (failure: unknown) =>
  failure instanceof RequestContractError ||
  (failure instanceof CoreError && failure.status >= 400 && failure.status < 500);

/**
 * What became of the last reset. `unconfirmed` is a lost or unreadable answer, or a 5xx: the
 * core may have forgotten the pin, so the pin read made after `at` decides.
 */
type Outcome = { kind: "refused"; text: string } | { kind: "unconfirmed"; at: number } | null;

/**
 * The certificate a Bambu Test connection pinned, and its deliberate reset. The reset lives
 * here, not in the dialog, so its outcome survives Cancel or Escape while it is in flight.
 * Every reset reads the pin again once it settles. After an unconfirmed reset, Reset stays off
 * until that read answers.
 */
function BambuPin({ integration, offline, onMessage }: { integration: Integration; offline: boolean; onMessage: (text: string) => void }) {
  const client = useQueryClient();
  const query = pinQuery(integration.id);
  const pin = useQuery(query);
  const [confirming, setConfirming] = useState(false);
  const [sending, setSending] = useState(false);
  const [outcome, setOutcome] = useState<Outcome>(null);
  const sendingNow = useRef(false);
  const reasonId = useId();
  const checking = outcome?.kind === "unconfirmed" && !(pin.isSuccess && pin.dataUpdatedAt >= outcome.at);
  const pinned = pin.data?.pinned === true && !pin.isError;
  const armed = pinned && !sending && !offline && !checking;

  const reset = async () => {
    if (!armed || sendingNow.current) return;
    sendingNow.current = true;
    setSending(true);
    setOutcome(null);
    try {
      await writeCore("resetTlsPin", { id: integration.id });
      client.setQueryData(query.queryKey, { pinned: false });
      setConfirming(false);
      onMessage(`Certificate reset for ${integration.name}. The next Test connection pins the certificate the printer shows.`);
    } catch (failure) {
      setOutcome(
        refused(failure)
          ? { kind: "refused", text: `The certificate was not reset: ${why(failure)}.` }
          : { kind: "unconfirmed", at: Date.now() },
      );
    } finally {
      sendingNow.current = false;
      setSending(false);
      // Whether or not the dialog is still open, read what the core now holds.
      void client.invalidateQueries({ queryKey: query.queryKey });
    }
  };
  const open = () => {
    if (offline || checking) return;
    setOutcome(null);
    setConfirming(true);
  };

  let text: string;
  if (pin.isError) {
    text = `Certificate state unavailable: ${why(pin.error)}.`;
  } else if (pin.data?.pinned) {
    text = ["Certificate pinned", pin.data.sha256_prefix, dateText(pin.data.pinned_at ?? null)].filter(Boolean).join(" · ");
  } else if (pin.data) {
    text = "Not pinned. Test connection to pin.";
  } else {
    text = "Checking the certificate.";
  }
  let note = "";
  if (outcome?.kind === "refused") note = outcome.text;
  else if (outcome?.kind === "unconfirmed") {
    note = checking
      ? "The core didn't confirm the reset. Checking the current pin."
      : pinned
        ? "The core didn't confirm the reset, and the certificate is still pinned."
        : "The core didn't confirm the reset. The certificate is no longer pinned.";
  }
  return (
    <span className="pp-fact-detail" data-tls-pin={integration.id}>
      {text}
      {pin.isError && (
        <button type="button" className="pp-button" onClick={() => void pin.refetch()}>
          Check the certificate again
        </button>
      )}
      {note && !confirming && <span role="alert">{note}</span>}
      {pinned && (
        <button
          type="button"
          className="pp-button"
          aria-disabled={offline || checking || undefined}
          onClick={open}
        >
          Reset certificate
        </button>
      )}
      {confirming && (
        <Dialog title={`Reset the certificate for ${integration.name}?`} onClose={() => setConfirming(false)}>
          <p>
            Print Partner forgets the pinned certificate. The next Test connection trusts whatever
            certificate answers at this printer's address, so run it only while you are sure the
            printer on your network is this same device.
          </p>
          {offline && <p id={reasonId}>{OFFLINE_REASON}</p>}
          {sending && <p role="status">Resetting the certificate.</p>}
          {note && <p role="alert" className="pp-field-error">{note}</p>}
          <div className="pp-dialog-actions">
            <button type="button" className="pp-button" onClick={() => setConfirming(false)}>
              {sending || outcome ? "Close" : "Cancel"}
            </button>
            {pinned && (
              <button
                type="button"
                className="pp-button pp-button-primary"
                aria-disabled={!armed || undefined}
                aria-describedby={offline ? reasonId : undefined}
                onClick={() => void reset()}
              >
                Reset certificate
              </button>
            )}
          </div>
        </Dialog>
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
