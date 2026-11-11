import type { Health } from "@print-partner/rust-client";
import { useIsMutating, useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState } from "react";
import { healthQuery, type ModuleName, type ModuleStatus } from "../core/modules";
import { writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

/** What each Module adds when it is on, in the order GET /health lists them. */
const MODULE_WORDS: Record<ModuleName, { name: string; adds: string }> = {
  farm: { name: "Farm", adds: "Printers, the send queue and the farm log" },
  spoolman: { name: "Spoolman", adds: "Filament stock from Spoolman and shortfalls per Build" },
  orders: { name: "Orders", adds: "Orders and due dates on Builds" },
  bambu: { name: "Bambu", adds: "Bambu printers on the local network" },
  assembly_tracking: { name: "Assembly tracking", adds: "Assembly progress for printed parts" },
};

/** Why a Module is off, from the reason codes in docs/desktop-rebuild.md. */
const OFF_REASONS: Record<Extract<ModuleStatus, { state: "off" }>["reason"], string> = {
  default_off: "Off by default. You have not turned it on.",
  turned_off: "You turned it off.",
  setting_unreadable: "Off because its setting could not be read. The core tries again.",
};

const MODULE_CHANGE_KEY = ["core", "set-module"] as const;

type Change = { module: ModuleName; on: boolean };

/** Writes one absolute state; the response and subsequent reads remain authoritative. */
export function ModuleList({ modules }: { modules: NonNullable<Health["modules"]> }) {
  const client = useQueryClient();
  const connection = useConnection();
  const unavailableId = useId();
  const [choice, setChoice] = useState<Change | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const sending = useRef(false);
  const writing = useIsMutating({ mutationKey: MODULE_CHANGE_KEY }) > 0;
  const writingNow = () => sending.current || client.isMutating({ mutationKey: MODULE_CHANGE_KEY }) > 0;
  const change = useMutation({
    mutationKey: MODULE_CHANGE_KEY,
    retry: false,
    networkMode: "always",
    mutationFn: async (target: Change) => {
      // A slow pre-change health read must not replace the write's returned state.
      await client.cancelQueries({ queryKey: healthQuery.queryKey });
      return writeCore("setModule", { module: target.module }, { on: target.on });
    },
    onSuccess: (answer) => {
      client.setQueryData<Health>(healthQuery.queryKey, (health) =>
        health ? { ...health, modules: answer.modules } : health,
      );
      setMessage("Module settings saved.");
    },
    onError: (failure: Error) => {
      setError(`Could not confirm the Module change: ${reason(failure)}. Reload Module state before trying again.`);
    },
    onSettled: () => {
      sending.current = false;
      setChoice(null);
      // A lost reply may still have applied. Reconcile reads; never retry a write automatically.
      void client.invalidateQueries({ queryKey: ["core"] });
    },
  });
  const unavailable = connection !== "online";
  const choiceUnavailable = choice !== null && !modules.some((m) =>
    m.module === choice.module && !(m.state === "off" && m.reason === "setting_unreadable"),
  );
  const confirm = () => {
    if (!choice || unavailable || choiceUnavailable || writingNow()) return;
    sending.current = true;
    setMessage("");
    setError("");
    change.mutate(choice);
  };
  if (modules.length === 0) return <p>The core lists no Modules.</p>;
  return (
    <>
      {unavailable && <p id={unavailableId}>{connection === "offline" ? OFFLINE_REASON : "Waiting for the core before changing Modules."}</p>}
      {writing && !choice && <p role="status">Saving Module state.</p>}
      {message && <p role="status">{message}</p>}
      {error && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{error}</p>
          <button type="button" className="pp-button" onClick={() => void client.invalidateQueries({ queryKey: ["core"] })}>
            Reload Module state
          </button>
        </div>
      )}
      <dl className="pp-facts pp-modules">
        {modules.map((m) => {
          const words = MODULE_WORDS[m.module];
          const unreadable = m.state === "off" && m.reason === "setting_unreadable";
          const blocked = unavailable || writing || unreadable;
          return (
            <div key={m.module} className="pp-fact" data-module={m.module} data-module-state={m.state}>
              <dt>{words.name}</dt>
              <dd>
                {m.state === "on" ? "On" : "Off"}
                {m.state === "off" && <span id={`${unavailableId}-${m.module}`} className="pp-fact-detail">{OFF_REASONS[m.reason]}</span>}
                <span className="pp-fact-detail">{words.adds}</span>
                <button
                  type="button"
                  className="pp-button"
                  aria-disabled={blocked || undefined}
                  aria-describedby={unavailable ? unavailableId : unreadable ? `${unavailableId}-${m.module}` : undefined}
                  onClick={() => {
                    if (!blocked && !writingNow()) setChoice({ module: m.module, on: m.state !== "on" });
                  }}
                >
                  Turn {words.name} {m.state === "on" ? "off" : "on"}
                </button>
              </dd>
            </div>
          );
        })}
      </dl>
      {choice && (
        <Dialog title={`Turn ${MODULE_WORDS[choice.module].name} ${choice.on ? "on" : "off"}?`} onClose={() => setChoice(null)}>
          <p>Saved Builds and history stay in place.</p>
          {!choice.on && <p>Turning this off does not stop a printer or undo completed work.</p>}
          {writing && <p role="status">Saving Module state.</p>}
          {unavailable && <p>{OFFLINE_REASON}</p>}
          {choiceUnavailable && <p>The core cannot read this Module's setting yet.</p>}
          <div className="pp-actions">
            <button type="button" className="pp-button" onClick={() => setChoice(null)}>{writing ? "Close" : "Cancel"}</button>
            <button type="button" className="pp-button pp-button-primary" aria-disabled={unavailable || choiceUnavailable || writing || undefined} onClick={confirm}>
              Confirm Module change
            </button>
          </div>
        </Dialog>
      )}
    </>
  );
}
