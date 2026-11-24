import type { IntegrationView } from "@print-partner/rust-client";
import { useId, useRef, useState, type FormEvent } from "react";
import { CoreError } from "../core/client";
import { RequestContractError, writeCoreAnswer } from "../core/rust";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON } from "../frame/connection";
import { shownAddress } from "./connections";

export type SpoolmanAction = { kind: "add" } | { kind: "edit" | "remove"; integration: IntegrationView };

type Recovery = "changed" | "missing" | "uncertain";

const recoveryText: Record<Recovery, string> = {
  changed: "This integration changed while you were editing it.",
  missing: "This integration was removed while you were viewing it.",
  uncertain: "Print Partner couldn't confirm the change.",
};

const fieldNames: Record<string, string> = { "/name": "Name", "/base_url": "Base URL", "/api_key": "API key" };
const writeOptions = { redirect: "error" } as const;
const addressOf = (integration: IntegrationView | null) => shownAddress(integration?.base_url ?? null)?.text ?? "";

function writeSpoolman(action: SpoolmanAction, form: FormData) {
  if (action.kind === "remove") return writeCoreAnswer("deleteIntegration", { id: action.integration.id }, writeOptions);
  const name = String(form.get("name") ?? "").trim();
  const address = String(form.get("base_url") ?? "");
  const apiKey = String(form.get("api_key") ?? "");
  const apiKeyPatch = form.has("clear_api_key") ? null : apiKey || undefined;
  if (action.kind === "add") {
    return writeCoreAnswer("createIntegration", {}, writeOptions, {
      kind: "spoolman", name, base_url: address.trim(), api_key: apiKeyPatch,
    });
  }
  const savedAddress = addressOf(action.integration);
  return writeCoreAnswer("updateIntegration", { id: action.integration.id }, writeOptions, {
    expected_revision: action.integration.revision,
    name: name !== action.integration.name ? name : undefined,
    base_url: address === savedAddress || address.trim() === savedAddress ? undefined : address.trim(),
    api_key: apiKeyPatch,
  });
}

function SpoolmanFields({ integration, busy }: { integration: IntegrationView | null; busy: boolean }) {
  const id = useId();
  const savedAddress = addressOf(integration);
  const [address, setAddress] = useState(savedAddress);
  const [clearKey, setClearKey] = useState(false);
  const moved = address !== savedAddress && address.trim() !== savedAddress;
  const needsKey = Boolean(integration?.has_api_key && moved && !clearKey);
  return (
    <div className="pp-fields">
      <div className="pp-field">
        <label htmlFor={`${id}-name`}>Name</label>
        <input id={`${id}-name`} name="name" defaultValue={integration?.name} disabled={busy} required maxLength={200} />
      </div>
      <div className="pp-field">
        <label htmlFor={`${id}-url`}>Base URL</label>
        <input id={`${id}-url`} name="base_url" type="url" placeholder="http://spoolman:7912" disabled={busy}
          required={!integration || moved} value={address} onChange={(event) => setAddress(event.target.value)} />
      </div>
      <div className="pp-field">
        <label htmlFor={`${id}-key`}>API key{needsKey ? "" : " (optional)"}</label>
        <input id={`${id}-key`} name="api_key" type="password" autoComplete="off" maxLength={4096}
          disabled={clearKey || busy} required={needsKey} aria-describedby={integration?.has_api_key ? `${id}-key-hint` : undefined} />
        {integration?.has_api_key && (
          <span id={`${id}-key-hint`} className="pp-fact-detail">
            {clearKey ? "Saving will clear the API key." : moved ? "Enter the API key for the new URL or clear the saved key."
              : "An API key is saved. Leave this blank to keep it."}
          </span>
        )}
      </div>
      {integration?.has_api_key && (
        <label>
          <input type="checkbox" name="clear_api_key" checked={clearKey} disabled={busy}
            onChange={(event) => setClearKey(event.target.checked)} />
          Clear saved API key
        </label>
      )}
    </div>
  );
}

export function SpoolmanDialog({ action, offline, onClose, onSaved, refresh, onRecovered }: {
  action: SpoolmanAction;
  offline: boolean;
  onClose: () => void;
  onSaved: (view: IntegrationView) => void;
  refresh: () => Promise<unknown>;
  onRecovered: (message: string) => void;
}) {
  const integration = action.kind === "add" ? null : action.integration;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [recovery, setRecovery] = useState<Recovery | null>(null);
  const running = useRef(false);
  const missingRevision = action.kind === "edit" && !integration?.revision;
  const close = () => {
    if (running.current) return;
    if (recovery) {
      onRecovered(`${recoveryText[recovery]} Review the current integrations before making another change.`);
    } else {
      onClose();
    }
  };

  const reload = async (outcome: Recovery) => {
    setError("");
    try {
      await refresh();
      onRecovered(`${recoveryText[outcome]} The list is up to date. Review it before making another change.`);
    } catch {
      setError("The current integrations couldn't be loaded. Reload the list to check what is saved.");
    }
  };

  const run = async (task: () => Promise<void>) => {
    if (offline || running.current) return;
    running.current = true;
    setBusy(true);
    try {
      await task();
    } finally {
      running.current = false;
      setBusy(false);
    }
  };

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (recovery || missingRevision) return;
    const form = new FormData(event.currentTarget);
    void run(async () => {
      setError("");
      try {
        const answer = await writeSpoolman(action, form);
        if (answer.status !== 200 || answer.redirected) throw new Error("Unconfirmed integration write");
        onSaved(answer.data);
      } catch (failure) {
        if (failure instanceof RequestContractError) {
          setError(failure.message);
          return;
        }
        let outcome: Recovery = "uncertain";
        if (failure instanceof CoreError && failure.problem && !failure.redirected && failure.status < 500) {
          if (failure.code === "integration_changed") {
            outcome = "changed";
          } else if (failure.code === "integration_not_found") {
            outcome = "missing";
          } else {
            setError(failure.errors.length
              ? failure.errors.map(({ pointer, detail }) => `${fieldNames[pointer] ?? pointer} ${detail}`).join(". ")
              : failure.detail ?? "The core refused the change.");
            return;
          }
        }
        setRecovery(outcome);
        await reload(outcome);
      }
    });
  };

  const removing = action.kind === "remove";
  return (
    <Dialog title={removing ? `Remove ${action.integration.name}?` : action.kind === "edit" ? "Edit Spoolman" : "Add Spoolman"} onClose={close}>
      <form onSubmit={submit}>
        {recovery ? (
          <p role="status">{recoveryText[recovery]} {busy && "Reloading integrations."}</p>
        ) : removing ? (
          <p>Remove this Spoolman connection and its saved API key from Print Partner?</p>
        ) : (
          <SpoolmanFields integration={integration} busy={busy} />
        )}
        {offline && <p>{OFFLINE_REASON}</p>}
        {missingRevision && <p role="alert">Close this form and reload integrations before editing.</p>}
        {error && <p role="alert" className="pp-field-error">{error}</p>}
        <div className="pp-dialog-actions">
          <button type="button" className="pp-button" disabled={busy} onClick={close}>{recovery ? "Close" : "Cancel"}</button>
          {recovery ? (
            <button type="button" className="pp-button pp-button-primary" disabled={offline || busy}
              onClick={() => void run(() => reload(recovery))}>Reload integrations</button>
          ) : (
            <button type="submit" className="pp-button pp-button-primary" disabled={offline || busy || missingRevision}>
              {removing ? "Remove Spoolman" : "Save Spoolman"}
            </button>
          )}
        </div>
      </form>
    </Dialog>
  );
}
