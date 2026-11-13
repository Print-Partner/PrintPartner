import type { RouteRequest, RouteResponse } from "@print-partner/rust-client";
import { useQuery, useQueryClient, type QueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useId, useState, type ComponentProps, type FormEvent, type ReactNode } from "react";
import { CoreError } from "../core/client";
import { readCore, writeCore } from "../core/rust";
import { reason, resendable } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

type Printer = RouteResponse<"listPrinters">["printers"][number];
type Integration = RouteResponse<"createIntegration">;
type NewIntegration = RouteRequest<"createIntegration">;
type IntegrationPatch = RouteRequest<"updateIntegration">;
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

/** `GET /api/v2/integrations`: every connection, without its secrets. */
const integrationsQuery = {
  queryKey: ["core", "integrations"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listIntegrations", {}, signal),
} as const;

/** The forms' names for request fields, by the JSON Pointer the core answers. */
const FIELD_NAMES: Record<string, string> = {
  "/name": "Name",
  "/base_url": "Address",
  "/api_key": "API key",
  "/username": "Username",
  "/password": "Password",
  "/model": "Model",
};

/** The core's own reason. A refusal that names fields gives each field with its reason. */
const why = (failure: unknown) =>
  failure instanceof CoreError && failure.errors.length > 0
    ? failure.errors.map(({ pointer, detail }) => `${FIELD_NAMES[pointer] ?? pointer} ${detail}`).join("; ")
    : reason(failure instanceof Error ? failure : new Error(String(failure)));

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

/** A probe passes when something answered there as the chosen kind of printer. */
const answered = (result: Probe | null) => result !== null && result.reachable && result.kind !== null;

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
 * nothing is resent or deleted. The integration write can lose its answer too. It is then
 * kept in `unsure`, and the next save or cancel reads the integrations list before
 * anything is sent: an unlinked integration with the same kind, name and address is the
 * one that was saved, and is used or discarded instead of creating a second.
 */
function AddPrinter({ onClose, onDone }: { onClose: () => void; onDone: (text: string) => void }) {
  const client = useQueryClient();
  const [kind, setKind] = useState<Kind>("moonraker");
  const [address, setAddress] = useState("");
  const [probe, setProbe] = useState<{ of: string; result: Probe } | null>(null);
  const [saved, setSaved] = useState<Integration | null>(null);
  const [unsure, setUnsure] = useState<NewIntegration | null>(null);
  // The list could not be read to see what was saved; the form may then be closed.
  const [unchecked, setUnchecked] = useState(false);
  const write = useWrite();
  const kindId = useId();
  const target = `${kind} ${address.trim()}`;
  const checked = probe?.of === target ? probe.result : null;
  const passed = answered(checked);
  const held = saved !== null || unsure !== null;
  const mayAdd = held || checked !== null;
  const unread = (what: string, failure: unknown) => {
    setUnchecked(true);
    return new Said(`Could not check whether the ${what} was saved: ${why(failure)}. Nothing was sent or deleted. Close this form and reopen Settings to see what was saved.`);
  };

  /** The printer that links the saved integration, if one does. A failed read stops the action. */
  const linkedPrinter = async (integration: Integration) => {
    try {
      return (await freshPrinters(client)).find((printer) => printer.integration?.id === integration.id);
    } catch (failure) {
      throw unread("printer", failure);
    }
  };
  /** The integration an unanswered create saved, if it did. A failed read stops the action. */
  const savedAfterAll = async (sent: NewIntegration) => {
    try {
      const [connections, printers] = await Promise.all([
        client.fetchQuery({ ...integrationsQuery, staleTime: 0 }),
        freshPrinters(client),
      ]);
      return connections.integrations.find(
        (one) =>
          one.kind === sent.kind && one.name === sent.name && one.base_url === sent.base_url &&
          !printers.some((printer) => printer.integration?.id === one.id),
      );
    } catch (failure) {
      throw unread("connection", failure);
    }
  };
  const create = async (body: NewIntegration) => {
    const found = unsure && (await savedAfterAll(unsure));
    if (found) return found;
    try {
      return await writeCore("createIntegration", {}, body);
    } catch (failure) {
      if (!(failure instanceof Error && resendable(failure))) throw failure;
      setUnsure(body);
      throw new Said(`The connection may have been saved: ${why(failure)}. Save again and this form checks before it sends anything, or cancel to discard it.`);
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
        (await create(unsure ?? {
          kind,
          name: text("name"),
          base_url: address.trim(),
          ...(kind === "moonraker"
            ? { api_key: text("api_key") || undefined }
            : { username: text("username") || undefined, password: text("password") || undefined }),
        }));
      setSaved(integration);
      setUnsure(null);
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
      const connection = saved ?? (unsure && (await savedAfterAll(unsure)));
      if (!connection) return onClose();
      const added = await linkedPrinter(connection);
      if (added) return onDone(`${added.name} was saved after all, so its connection was kept. Delete the printer if you do not want it.`);
      await writeCore("deleteIntegration", { id: connection.id });
      onClose();
    });

  return (
    // With a connection saved or possibly saved and no printer, only "Save printer" or
    // "Discard" closes this, unless the lists could not be read to check what was saved.
    <Dialog title="Add a printer" onClose={() => (!held || unchecked) && onClose()}>
      <form onSubmit={add}>
        <fieldset disabled={held}>
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
          <button type="button" className="pp-button" onClick={() => void (held ? discard() : onClose())}>
            {held ? "Discard" : "Cancel"}
          </button>
          <button type="submit" className="pp-button pp-button-primary" aria-disabled={!mayAdd || write.busy || undefined}>
            {held || passed || !checked ? "Save printer" : "Save anyway"}
          </button>
        </div>
        {!mayAdd && <p>Check the address before saving.</p>}
      </form>
    </Dialog>
  );
}

/** The secret a kind signs in with, and whether one is stored. */
const secretOf = (stored: Integration) =>
  stored.kind === "prusalink"
    ? { field: "password" as const, word: "password", one: "A password", held: stored.has_password }
    : { field: "api_key" as const, word: "API key", one: "An API key", held: stored.has_api_key };

/** What the form changes on the connection: only what the owner touched, never a value it was not given. */
function connectionPatch(form: FormData, stored: Integration): IntegrationPatch {
  const text = (name: string) => String(form.get(name) ?? "").trim();
  const secret = secretOf(stored);
  const patch: IntegrationPatch = {};
  if (text("address") !== (stored.base_url ?? "")) patch.base_url = text("address");
  if (form.get("remove_secret")) patch[secret.field] = null;
  else if (text("secret")) patch[secret.field] = text("secret");
  if (stored.kind === "prusalink" && text("username") !== (stored.username ?? "")) patch.username = text("username") || null;
  return patch;
}

/** The address and credentials of a Moonraker or PrusaLink connection. A secret is write-only. */
function ConnectionFields({ stored, locked }: { stored: Integration; locked: boolean }) {
  const [address, setAddress] = useState(stored.base_url ?? "");
  const secret = secretOf(stored);
  const moved = address.trim() !== (stored.base_url ?? "");
  return (
    <fieldset disabled={locked}>
      <legend>Connection</legend>
      <div className="pp-fields">
        <Field label="Address" name="address" type="url" required value={address} onChange={(event) => setAddress(event.target.value)} />
        {stored.kind === "prusalink" && <Field label="Username" name="username" autoComplete="off" defaultValue={stored.username ?? ""} />}
        <Field
          label={`New ${secret.word}`}
          name="secret"
          type="password"
          autoComplete="off"
          hint={secret.held ? `${secret.one} is set. Leave this empty to keep it.` : `No ${secret.word} is set.`}
        />
      </div>
      {secret.held && (
        <label>
          <input type="checkbox" name="remove_secret" /> Remove the stored {secret.word}
        </label>
      )}
      {moved && secret.held && (
        <p role="status">The address changed, so enter the {secret.word} again or remove it: a stored {secret.word} is never sent to a new address.</p>
      )}
    </fieldset>
  );
}

/** Why a printer's connection cannot be edited right now. Its name still can. */
function ConnectionNote({ printer, connections }: { printer: Printer; connections: UseQueryResult<RouteResponse<"listIntegrations">> }) {
  if (!printer.integration) return <p>This printer has no connection. Only its name can be changed.</p>;
  if (connections.isPending) return <p role="status">Loading the connection.</p>;
  if (!connections.isError) return <p>This printer's connection cannot be edited here. Only its name can be changed.</p>;
  return (
    <div role="alert" className="pp-banner" data-kind="error">
      <p>Could not load the connection: {why(connections.error)}. Only the name can be changed.</p>
      <button type="button" className="pp-button" onClick={() => void connections.refetch()}>Reload connection</button>
    </div>
  );
}

/**
 * Edit is the connection (address and credentials), then the printer's name. The
 * connection goes first, so a refused connection change leaves the name as it was. A key
 * or password is write-only: the form says whether one is stored and never shows it. When
 * the address changes, the core takes a stored secret only if it is sent again or removed,
 * so the form asks for it and says why. Saving without it shows the core's refusal.
 */
function EditPrinter({ printer, onClose, onDone }: { printer: Printer; onClose: () => void; onDone: (text: string) => void }) {
  const connections = useQuery(integrationsQuery);
  const [connectionSaved, setConnectionSaved] = useState(false);
  const write = useWrite();
  const stored = connections.data?.integrations.find(
    (one) => one.id === printer.integration?.id && (one.kind === "moonraker" || one.kind === "prusalink"),
  );
  const save = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const name = String(form.get("name") ?? "").trim();
    const patch = stored && !connectionSaved ? connectionPatch(form, stored) : {};
    void write.run(async () => {
      if (stored && Object.keys(patch).length > 0) {
        await writeCore("updateIntegration", { id: stored.id }, patch);
        setConnectionSaved(true);
      }
      if (name !== printer.name) {
        await writeCore("updatePrinter", { id: printer.id }, { name }).catch((failure: unknown) => {
          throw new Said(`The name was not saved: ${why(failure)}. Any change to the connection was saved.`);
        });
      }
      onDone(`Saved ${name}.`);
    });
  };
  const loading = printer.integration !== null && connections.isPending;
  return (
    <Dialog title={`Edit ${printer.name}`} onClose={onClose}>
      <form onSubmit={save}>
        <div className="pp-fields">
          <Field label="Name" name="name" required maxLength={200} defaultValue={printer.name} />
        </div>
        {stored ? <ConnectionFields stored={stored} locked={connectionSaved} /> : <ConnectionNote printer={printer} connections={connections} />}
        {write.error && <p role="alert" className="pp-field-error">{write.error}</p>}
        <div className="pp-dialog-actions">
          <button type="button" className="pp-button" onClick={onClose}>{connectionSaved ? "Close" : "Cancel"}</button>
          <button type="submit" className="pp-button pp-button-primary" aria-disabled={write.busy || loading || undefined}>
            Save changes
          </button>
        </div>
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

/** The saved printers, with add, edit and delete. */
export function PrintersSection() {
  const client = useQueryClient();
  const connection = useConnection();
  const offline = connection !== "online";
  const printers = useQuery(printersQuery);
  const [dialog, setDialog] = useState<"add" | { edit: Printer } | { remove: Printer } | null>(null);
  const [message, setMessage] = useState("");
  const list = printers.data?.printers;
  // A write may have applied even when its answer was lost, so every close reads again.
  const close = (text = "") => {
    setDialog(null);
    setMessage(text);
    void client.invalidateQueries({ queryKey: ["core", "printers"] });
    void client.invalidateQueries({ queryKey: integrationsQuery.queryKey });
  };
  const open = (next: NonNullable<typeof dialog>) => {
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
              <button type="button" className="pp-button" aria-disabled={offline || undefined} onClick={() => open({ edit: printer })}>
                Edit {printer.name}
              </button>
              <button type="button" className="pp-button" aria-disabled={offline || undefined} onClick={() => open({ remove: printer })}>
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
      {dialog && dialog !== "add" && "edit" in dialog && <EditPrinter printer={dialog.edit} onClose={() => close()} onDone={close} />}
      {dialog && dialog !== "add" && "remove" in dialog && (
        <RemovePrinter
          printer={dialog.remove}
          shared={dialog.remove.integration !== null && (list ?? []).some((other) => other.id !== dialog.remove.id && other.integration?.id === dialog.remove.integration?.id)}
          onClose={() => close()}
          onRemoved={close}
        />
      )}
    </>
  );
}
