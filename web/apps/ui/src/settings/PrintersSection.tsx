import type { RouteResponse } from "@print-partner/rust-client";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useId, useState, type ComponentProps, type FormEvent, type ReactNode } from "react";
import { readCore, writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

type Printer = RouteResponse<"listPrinters">["printers"][number];
type Integration = RouteResponse<"createIntegration">;
type Probe = RouteResponse<"probePrinter">;
type Kind = "moonraker" | "prusalink";

const KIND_NAMES = { moonraker: "Moonraker", prusalink: "PrusaLink", bambu: "Bambu" } as const;

/** The same cached `GET /api/v2/printers` the Farm screen reads. */
const printersQuery = {
  queryKey: ["core", "printers"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinters", {}, signal),
} as const;

/** The printers the core lists now, never the cached list. */
const freshPrinters = (client: QueryClient) =>
  client.fetchQuery({ ...printersQuery, staleTime: 0 }).then((list) => list.printers);

const why = (failure: unknown) => reason(failure instanceof Error ? failure : new Error(String(failure)));

function Field({ label, name, hint, ...input }: { label: string; name: string; hint?: string } & ComponentProps<"input">) {
  const id = useId();
  return (
    <div className="pp-field">
      <label htmlFor={id}>{label}</label>
      <input id={id} name={name} aria-describedby={hint ? `${id}-hint` : undefined} {...input} />
      {hint && <span id={`${id}-hint`} className="pp-fact-detail">{hint}</span>}
    </div>
  );
}

/** A failure whose message is already a full sentence for the owner. */
class Said extends Error {}

/** Runs one write at a time and keeps its failure text. Nothing is resent automatically. */
function useWrite() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const run = async (write: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await write();
    } catch (failure) {
      setError(failure instanceof Said ? failure.message : why(failure));
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, run };
}


/**
 * Add is two writes: the integration (address and credentials), then the printer that
 * links it. If the printer write fails, the saved integration is kept in `saved` and the
 * owner chooses "Save printer", which sends only the printer, or "Discard", which deletes
 * that integration. A failed printer write may still have applied, so both choices read
 * the list first: when a printer already links the integration the add is done, and
 * nothing is resent or deleted.
 */
function AddPrinter({ onClose, onDone }: { onClose: () => void; onDone: (text: string) => void }) {
  const client = useQueryClient();
  const [kind, setKind] = useState<Kind>("moonraker");
  const [address, setAddress] = useState("");
  const [probe, setProbe] = useState<{ of: string; result: Probe } | null>(null);
  const [saved, setSaved] = useState<Integration | null>(null);
  // The list could not be read to see what was saved; the form may then be closed.
  const [unchecked, setUnchecked] = useState(false);
  const write = useWrite();
  const kindId = useId();
  const target = `${kind} ${address.trim()}`;
  const checked = probe?.of === target ? probe.result : null;
  const passed = checked !== null && checked.reachable && checked.kind !== null;
  const mayAdd = saved !== null || checked !== null;

  /** The printer that links the saved integration, if one does. A failed read stops the action. */
  const linkedPrinter = async (integration: Integration) => {
    try {
      return (await freshPrinters(client)).find((printer) => printer.integration?.id === integration.id);
    } catch (failure) {
      setUnchecked(true);
      throw new Said(`Could not check whether the printer was saved: ${why(failure)}. Nothing was sent or deleted. Close this form and reopen Settings to see what was saved.`);
    }
  };
  const check = () =>
    write.run(async () => {
      const result = await writeCore("probePrinter", {}, { kind, address: address.trim() });
      setProbe({ of: target, result });
    });
  const add = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!mayAdd) return;
    const form = new FormData(event.currentTarget);
    const text = (name: string) => String(form.get(name) ?? "").trim();
    const mm = (name: string) => Number(form.get(name));
    void write.run(async () => {
      const added = saved && (await linkedPrinter(saved));
      if (added) return onDone(`${added.name} was saved after all.`);
      const integration =
        saved ??
        (await writeCore("createIntegration", {}, {
          kind,
          name: text("name"),
          base_url: address.trim(),
          ...(kind === "moonraker"
            ? { api_key: text("api_key") || undefined }
            : { username: text("username") || undefined, password: text("password") || undefined }),
        }));
      setSaved(integration);
      try {
        await writeCore("createPrinter", {}, {
          name: text("name"),
          model: text("model"),
          bed_width_mm: mm("bed_width_mm"),
          bed_depth_mm: mm("bed_depth_mm"),
          bed_height_mm: mm("bed_height_mm"),
          integration_id: integration.id,
        });
      } catch (failure) {
        throw new Said(`The connection was saved, but the printer was not: ${why(failure)}. Save the printer again, or discard the connection.`);
      }
      onDone(`Added ${text("name")}.`);
    });
  };
  const discard = () =>
    write.run(async () => {
      if (!saved) return onClose();
      const added = await linkedPrinter(saved);
      if (added) return onDone(`${added.name} was saved after all, so its connection was kept. Delete the printer if you do not want it.`);
      await writeCore("deleteIntegration", { id: saved.id });
      onClose();
    });

  return (
    // With a connection saved and no printer, only "Save printer" or "Discard" closes this,
    // unless the list could not be read to check what was saved.
    <Dialog title="Add a printer" onClose={() => (saved === null || unchecked) && onClose()}>
      <form onSubmit={add}>
        <fieldset disabled={saved !== null}>
          <legend>Connection</legend>
          <div className="pp-fields">
            <div className="pp-field">
              <label htmlFor={kindId}>Kind</label>
              <select id={kindId} value={kind} onChange={(event) => setKind(event.target.value as Kind)}>
                <option value="moonraker">Moonraker</option>
                <option value="prusalink">PrusaLink</option>
              </select>
            </div>
            <Field label="Address" name="address" type="url" required placeholder="http://printer.local" value={address} onChange={(event) => setAddress(event.target.value)} />
            {kind === "moonraker" ? (
              <Field label="API key (optional)" name="api_key" type="password" autoComplete="off" />
            ) : (
              <>
                <Field label="Username" name="username" autoComplete="off" />
                <Field label="Password" name="password" type="password" autoComplete="off" />
              </>
            )}
          </div>
          <p>A key or password is checked when the printer first connects, not here. It is never shown again.</p>
          <button type="button" className="pp-button" aria-disabled={!address.trim() || write.busy || undefined} onClick={() => address.trim() && void check()}>
            Check address
          </button>
        </fieldset>
        {checked && <p role="status">{passed ? "The printer answered." : `${checked.reason} You can save it anyway, for a printer that is switched off.`}</p>}
        <div className="pp-fields">
          <Field label="Name" name="name" required maxLength={200} />
          <Field label="Model" name="model" required maxLength={200} />
          {(["width", "depth", "height"] as const).map((side) => (
            <Field key={side} label={`Bed ${side} (mm)`} name={`bed_${side}_mm`} type="number" required min={1} max={10000} step={1} />
          ))}
        </div>
        {write.error && <p role="alert" className="pp-field-error">{write.error}</p>}
        <div className="pp-dialog-actions">
          {unchecked && <button type="button" className="pp-button" onClick={onClose}>Close</button>}
          <button type="button" className="pp-button" onClick={() => void (saved ? discard() : onClose())}>
            {saved ? "Discard" : "Cancel"}
          </button>
          <button type="submit" className="pp-button pp-button-primary" aria-disabled={!mayAdd || write.busy || undefined}>
            {saved || passed || !checked ? "Save printer" : "Save anyway"}
          </button>
        </div>
        {!mayAdd && <p>Check the address before saving.</p>}
      </form>
    </Dialog>
  );
}

/**
 * Delete is the printer, then its integration when no other printer links it. The core
 * does not refuse to delete an integration a printer still links, and the listed printers
 * can be out of date, so the list is read again before that second write: the integration
 * is kept when another printer links it, or when that read fails. If the second write
 * fails the dialog says the printer is gone and offers that write again.
 */
function RemovePrinter({ printer, shared, onClose, onRemoved }: { printer: Printer; shared: boolean; onClose: () => void; onRemoved: (text: string) => void }) {
  const client = useQueryClient();
  const [printerGone, setPrinterGone] = useState(false);
  const write = useWrite();
  const link = shared ? null : printer.integration;
  const remove = () =>
    write.run(async () => {
      if (!printerGone) await writeCore("deletePrinter", { id: printer.id });
      setPrinterGone(true);
      const done = (kept = "") => onRemoved(`Deleted ${printer.name}.${kept}`);
      const connection = printer.integration;
      if (!connection) return done();
      const fresh = await freshPrinters(client).catch(() => null);
      if (!fresh) return done(" Its connection was kept, because the printers could not be read to check that no other printer uses it.");
      if (fresh.some((other) => other.id !== printer.id && other.integration?.id === connection.id)) {
        return done(" Its connection is used by another printer and was kept.");
      }
      try {
        await writeCore("deleteIntegration", { id: connection.id });
      } catch (failure) {
        throw new Said(`The printer was deleted, but its connection was not: ${why(failure)}.`);
      }
      done();
    });
  return (
    <Dialog title={`Delete ${printer.name}?`} onClose={onClose}>
      <p>
        {link
          ? `This deletes the printer and its ${KIND_NAMES[link.kind]} connection, with the saved address and credentials. The connection is kept if another printer uses it.`
          : shared
            ? "This deletes the printer. Its connection is used by another printer and stays."
            : "This deletes the printer."}
      </p>
      <p>Builds, history and finished work stay in place. Nothing is sent to the printer.</p>
      {write.error && <p role="alert" className="pp-field-error">{write.error}</p>}
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button" onClick={onClose}>{printerGone ? "Close" : "Cancel"}</button>
        <button type="button" className="pp-button pp-button-primary" aria-disabled={write.busy || undefined} onClick={() => void remove()}>
          {printerGone ? "Delete connection" : "Delete printer"}
        </button>
      </div>
    </Dialog>
  );
}

/** The saved printers, with add and delete. Editing one follows in its own change. */
export function PrintersSection() {
  const client = useQueryClient();
  const connection = useConnection();
  const offline = connection !== "online";
  const printers = useQuery(printersQuery);
  const [dialog, setDialog] = useState<"add" | Printer | null>(null);
  const [message, setMessage] = useState("");
  const list = printers.data?.printers;
  // A write may have applied even when its answer was lost, so every close reads again.
  const close = (text = "") => {
    setDialog(null);
    setMessage(text);
    void client.invalidateQueries({ queryKey: ["core", "printers"] });
  };
  const open = (next: "add" | Printer) => {
    if (offline) return;
    setMessage("");
    setDialog(next);
  };
  let body: ReactNode;
  if (!list) {
    body = printers.isError ? null : <p role="status">Loading printers.</p>;
  } else if (list.length === 0) {
    body = <p>No printers yet. Add one to see it on Farm.</p>;
  } else {
    body = (
      <dl className="pp-facts">
        {list.map((printer) => (
          <div key={printer.id} className="pp-fact" data-printer={printer.id}>
            <dt>{printer.name}</dt>
            <dd>
              {printer.model}
              <span className="pp-fact-detail">{printer.integration ? KIND_NAMES[printer.integration.kind] : "No connection"}</span>
              <button type="button" className="pp-button" aria-disabled={offline || undefined} onClick={() => open(printer)}>
                Delete {printer.name}
              </button>
            </dd>
          </div>
        ))}
      </dl>
    );
  }
  return (
    <>
      {connection === "offline" && <p>{OFFLINE_REASON}</p>}
      {message && <p role="status">{message}</p>}
      {printers.isError && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{list ? "Could not refresh the printers, so this list may be out of date" : "Could not load the printers"}: {why(printers.error)}</p>
          <button type="button" className="pp-button" onClick={() => void printers.refetch()}>Reload printers</button>
        </div>
      )}
      {body}
      {list && (
        <button type="button" className="pp-button" aria-disabled={offline || undefined} onClick={() => open("add")}>
          Add a printer
        </button>
      )}
      {dialog === "add" && <AddPrinter onClose={() => close()} onDone={close} />}
      {dialog && dialog !== "add" && (
        <RemovePrinter
          printer={dialog}
          shared={dialog.integration !== null && (list ?? []).some((other) => other.id !== dialog.id && other.integration?.id === dialog.integration?.id)}
          onClose={() => close()}
          onRemoved={close}
        />
      )}
    </>
  );
}
