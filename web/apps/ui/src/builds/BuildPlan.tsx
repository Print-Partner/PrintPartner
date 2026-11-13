import { KEEP_EDITS, SignedOutNote, signedOut } from "../auth/SignedOutNote";
import type {
  AcceptDraftReceipt,
  DraftAcceptPreview,
  DraftWorkspace,
} from "@print-partner/rust-client";
import {
  useIsMutating,
  useMutation,
  useMutationState,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useLocation, useNavigate, useOutletContext } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { newKey, resendable } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { NotFound } from "../screen/screens";
import { ModuleOff, MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { plural } from "../today/beds";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import { planKey, useMatchStep } from "./PlanReconcile";
import { DRAFT_OPEN, NO_DRAFT, PLATES_CLEARED } from "./planCopy";
import {
  acceptBody,
  fieldName,
  fieldsOf,
  headerOf,
  identityOf,
  includeChoice,
  quantityChoice,
  quantityValue,
  refusalText,
  sameHeader,
  valueText,
  type BuildHeader,
  type HeaderFields,
  type PartView,
  type PlanChoice,
  type PlanData,
  type PreviewChanges,
} from "./plan";

// History restores into the Plan cache by this key.
export { planKey };
const draftWriteKey = (id: number) => ["core", "plan-draft-write", id] as const;

/** The navigation state History hands the Plan tab after Restore as draft. */
export interface RestoredState {
  restored: { draftId: number; summary: string };
}

const restoredOf = (state: unknown): RestoredState["restored"] | null => {
  const restored = (state as Partial<RestoredState> | null)?.restored;
  return restored && typeof restored.draftId === "number" ? restored : null;
};

/**
 * The Build's header, the Accepted base and the open draft, if any. Every value the tab
 * shows comes from the Rust core; nothing here guesses the accepted Plan.
 */
export const planQuery = (id: number) => ({
  queryKey: planKey(id),
  queryFn: async ({ signal }: { signal: AbortSignal }): Promise<PlanData> => {
    const [drafts, { build, base }] = await Promise.all([
      readCore("listDrafts", { id }, signal),
      readCore("readBuildHeader", { id }, signal),
    ]);
    const open = drafts.drafts.find((d) => d.state === "open");
    if (!open) return { build, base, draft: null };
    const params = { id, draft_id: open.draft_id };
    const [workspace, header] = await Promise.all([
      readCore("readDraftWorkspace", params, signal),
      readCore("readDraftHeader", params, signal),
    ]);
    return { build: header.build, base, draft: { workspace, header: header.draft } };
  },
  retry: (failures: number, error: Error) =>
    !(error instanceof CoreError && error.status === 404) && failures < 3,
});

/** Draft writes run one at a time, each against the draft the last one left. */
function useDraftWrites(id: number) {
  const client = useQueryClient();
  const latest = () => client.getQueryData<PlanData>(planKey(id));
  // A resend must repeat its first body: the core replays a key only for the same request.
  const bodies = useRef(new Map<string, unknown>());
  const once = <T,>(key: string, build: () => T): T => {
    if (!bodies.current.has(key)) bodies.current.set(key, build());
    return bodies.current.get(key) as T;
  };
  const settle = (key: string) => {
    bodies.current.delete(key);
    return client.invalidateQueries({ queryKey: planKey(id) });
  };
  const scope = { id: `plan-draft-${id}` };
  const mutationKey = draftWriteKey(id);
  const header = useMutation({
    scope,
    mutationKey,
    // A 401 keeps the page, so the typed edit stays on screen (`SignedOutNote`).
    meta: KEEP_EDITS,
    mutationFn: ({ fields, key }: { fields: HeaderFields; key: string }) => {
      const body = once(key, () => {
        const data = latest();
        if (!data) throw new Error("The Plan has not loaded yet");
        return {
          expected_base: data.draft?.workspace.draft.base ?? data.base,
          expected_draft: data.draft ? identityOf(data.draft.workspace.draft) : null,
          expected_header_version: data.draft?.header?.header_version ?? null,
          header: headerOf(fields),
        };
      });
      return writeCore("saveDraftHeader", { id }, body, key);
    },
    retry: (failures: number, error: Error) => failures < 2 && resendable(error),
    onSettled: (_data, _error, { key }) => settle(key),
  });
  const parts = useMutation({
    scope,
    mutationKey,
    meta: KEEP_EDITS,
    mutationFn: ({ choice, key }: { choice: PlanChoice; key: string }) => {
      const body = once(key, () => {
        const draft = latest()?.draft;
        if (!draft) throw new Error("No draft is open");
        return {
          decisions: [choice],
          expected_base: draft.workspace.draft.base,
          expected_draft: identityOf(draft.workspace.draft),
          remap_checkoff_links: false,
        };
      });
      return writeCore("autosaveDraft", { id }, body, key);
    },
    retry: (failures: number, error: Error) => failures < 2 && resendable(error),
    // The next write in the queue names the draft this one left.
    onSuccess: ({ workspace }) =>
      client.setQueryData<PlanData>(planKey(id), (data) =>
        data?.draft ? { ...data, draft: { ...data.draft, workspace } } : data,
      ),
    onSettled: (_data, _error, { key }) => settle(key),
  });
  return { header, parts, mutationKey };
}

type Writes = ReturnType<typeof useDraftWrites>;

/** What the last write did, read out politely. */
function SaveStatus({ writes }: { writes: Writes }) {
  // Writes run one at a time, so the last submitted is the last settled.
  const last = writes.header.submittedAt >= writes.parts.submittedAt ? writes.header : writes.parts;
  const failed = last.error;
  const text =
    writes.header.isPending || writes.parts.isPending
      ? "Saving to the draft"
      : failed
        ? `${resendable(failed) ? "Save not confirmed" : "Not saved"}: ${refusalText(failed, "save")}`
        : last.isSuccess
          ? "Saved to the draft. The Build changes only when you accept."
          : "";
  return (
    <p role="status" className="pp-save-status" data-failed={failed ? "" : undefined}>
      {failed && signedOut(failed) ? <SignedOutNote /> : text}
    </p>
  );
}

const HEADER_FIELDS = [
  { field: "name", label: "Name", type: "text", orders: false },
  { field: "order_number", label: "Order number", type: "text", orders: true },
  { field: "customer", label: "Customer", type: "text", orders: true },
  { field: "due_date", label: "Due date", type: "date", orders: true },
] as const;

/**
 * The header form. Each field saves to the draft when it loses focus or on Enter. Once the
 * draft writes settle, the saved header replaces every field not typed in since the last
 * save was sent.
 */
function HeaderForm({ data, writes, busy }: { data: PlanData; writes: Writes; busy: boolean }) {
  const saved = data.draft?.header?.header ?? data.build;
  const [fields, setFields] = useState(() => fieldsOf(saved));
  const [shown, setShown] = useState(saved);
  const [typed, setTyped] = useState<ReadonlySet<string>>(new Set());
  const [sent, setSent] = useState<BuildHeader | null>(null);
  if (!busy && !sameHeader(saved, shown)) {
    const after = fieldsOf(saved);
    setShown(saved);
    setFields(
      Object.fromEntries(
        Object.entries(fields).map(([field, v]) => [
          field,
          typed.has(field) ? v : after[field as keyof HeaderFields],
        ]),
      ) as HeaderFields,
    );
  }
  const [invalid, setInvalid] = useState(false);
  const offline = useConnection() === "offline";
  const orders = useModule("orders");
  const ids = { form: useId(), reason: useId(), name: useId() };
  const commit = () => {
    const header = headerOf(fields);
    setInvalid(header.name === "");
    if (header.name === "" || sameHeader(header, sent ?? saved) || offline) return;
    setSent(header);
    setTyped(new Set());
    // Only the latest header write calls this, and only it can still be the one sent.
    writes.header.mutate(
      { fields, key: newKey() },
      { onSettled: () => setSent((current) => (current === header ? null : current)) },
    );
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") commit();
  };
  return (
    <section aria-labelledby={ids.form} className="pp-plan-header">
      <h2 id={ids.form}>Name and order</h2>
      <div className="pp-fields">
        {HEADER_FIELDS.filter((f) => !f.orders || orders?.state !== "off").map((f) => (
          <label key={f.field} className="pp-field">
            <span>{f.label}</span>
            <input
              type={f.type}
              value={fields[f.field]}
              readOnly={offline}
              aria-invalid={f.field === "name" && invalid ? true : undefined}
              aria-describedby={
                [offline && ids.reason, f.field === "name" && invalid && ids.name]
                  .filter(Boolean)
                  .join(" ") || undefined
              }
              onChange={(event) => {
                setFields({ ...fields, [f.field]: event.target.value });
                setTyped(new Set(typed).add(f.field));
              }}
              onBlur={commit}
              onKeyDown={onKeyDown}
            />
          </label>
        ))}
      </div>
      {invalid && (
        <p id={ids.name} className="pp-field-error">
          A Build needs a name.
        </p>
      )}
      {offline && (
        <p id={ids.reason} className="pp-mutation-reason">
          {OFFLINE_REASON}
        </p>
      )}
      {orders?.state === "off" && (
        <ModuleOff
          module="Orders"
          wouldShow="the order number, customer and due date"
          where="this form"
        />
      )}
    </section>
  );
}

/**
 * One Part row: whether it is included and its quantity override. While an Included save
 * is queued or running, the box shows its value, and the next click toggles from that.
 */
function PartRow({ part, writes, reasonId }: { part: PartView; writes: Writes; reasonId: string }) {
  const offline = useConnection() === "offline";
  const target = JSON.stringify(includeChoice(part, true).target);
  const pending = useMutationState({
    filters: { mutationKey: writes.mutationKey, status: "pending" },
    select: (mutation) => (mutation.state.variables as { choice?: PlanChoice } | undefined)?.choice,
  }).filter((choice) => choice && JSON.stringify(choice.target) === target);
  const queued = (kind: PlanChoice["kind"]) => pending.findLast((choice) => choice?.kind === kind);
  const queuedInclude = queued("set_included");
  const included = queuedInclude?.kind === "set_included" ? queuedInclude.value : part.included;
  const queuedQuantity = queued("set_quantity_override");
  const sentQuantity =
    queuedQuantity?.kind === "set_quantity_override"
      ? queuedQuantity.value
      : part.quantity_override;
  const [quantity, setQuantity] = useState(part.quantity_override?.toString() ?? "");
  const [shown, setShown] = useState(part.quantity_override);
  const [edited, setEdited] = useState(false);
  if (!queuedQuantity && part.quantity_override !== shown) {
    setShown(part.quantity_override);
    if (!edited) setQuantity(part.quantity_override?.toString() ?? "");
  }
  const [invalid, setInvalid] = useState(false);
  const errorId = useId();
  const save = (choice: PlanChoice) => writes.parts.mutate({ choice, key: newKey() });
  const commitQuantity = () => {
    const value = quantityValue(quantity);
    setInvalid(value === "invalid");
    if (value === "invalid" || offline) return;
    setEdited(false);
    if (value === sentQuantity) return;
    save(quantityChoice(part, value));
  };
  return (
    <tr>
      <th scope="row">
        {part.filename}
        <span className="pp-muted pp-path">{part.relative_path}</span>
      </th>
      <td>{part.source_layer}</td>
      <td>
        <input
          type="checkbox"
          checked={included}
          aria-label={`Include ${part.filename}`}
          aria-disabled={offline || undefined}
          aria-describedby={offline ? reasonId : undefined}
          onChange={() => {
            if (offline) return;
            save(includeChoice(part, !included));
          }}
        />
      </td>
      <td>
        <input
          className="pp-quantity num"
          inputMode="numeric"
          value={quantity}
          placeholder={String(part.quantity_inferred)}
          readOnly={offline}
          aria-label={`Quantity of ${part.filename}, blank for ${part.quantity_inferred} from the Source`}
          aria-invalid={invalid || undefined}
          aria-describedby={
            [invalid && errorId, offline && reasonId].filter(Boolean).join(" ") || undefined
          }
          onChange={(event) => {
            setQuantity(event.target.value);
            setEdited(true);
          }}
          onBlur={commitQuantity}
          onKeyDown={(event) => event.key === "Enter" && commitQuantity()}
        />
        {invalid && (
          <span id={errorId} className="pp-field-error">
            A whole number from 1 to 10,000, or blank.
          </span>
        )}
      </td>
    </tr>
  );
}

function PartsTable({ parts, writes }: { parts: PartView[]; writes: Writes }) {
  const headingId = useId();
  const reasonId = useId();
  const offline = useConnection() === "offline";
  if (parts.length === 0) return <p>This draft has no Parts.</p>;
  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId}>Parts</h2>
      {offline && (
        <p id={reasonId} className="pp-mutation-reason">
          {OFFLINE_REASON}
        </p>
      )}
      <table className="pp-plan-parts">
        <thead>
          <tr>
            <th scope="col">Part</th>
            <th scope="col">Layer</th>
            <th scope="col">Included</th>
            <th scope="col">Quantity</th>
          </tr>
        </thead>
        <tbody>
          {parts.map((part) => (
            <PartRow key={part.draft_part_id} part={part} writes={writes} reasonId={reasonId} />
          ))}
        </tbody>
      </table>
    </section>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  const id = useId();
  return (
    <section aria-labelledby={id} className="pp-gate-section">
      <h3 id={id}>{title}</h3>
      {children}
    </section>
  );
}

function List({ items, none }: { items: string[]; none: string }) {
  if (items.length === 0) return <p className="pp-muted">{none}</p>;
  return (
    <ul>
      {items.map((item, index) => (
        <li key={index}>{item}</li>
      ))}
    </ul>
  );
}

/** Everything Accept would change, in the order the user checks it. */
function Changes({ changes }: { changes: PreviewChanges }) {
  const units = changes.required_units;
  return (
    <>
      {changes.header.length > 0 && (
        <Section title="Name and order">
          <table className="pp-gate-header">
            <thead>
              <tr>
                <th scope="col">Field</th>
                <th scope="col">Now</th>
                <th scope="col">After Accept</th>
              </tr>
            </thead>
            <tbody>
              {changes.header.map((change) => (
                <tr key={change.field}>
                  <th scope="row">{fieldName(change.field)}</th>
                  <td>{valueText(change.old)}</td>
                  <td>{valueText(change.new)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </Section>
      )}
      <Section title="Parts">
        <List
          none="No Part changes."
          items={[
            ...changes.fields.map(
              (c) =>
                `${c.filename}: ${fieldName(c.field)} ${valueText(c.old)} to ${valueText(c.new)}`,
            ),
            ...changes.parts_added.map(
              (p) => `${p.filename}: added, quantity ${p.quantity_effective}`,
            ),
            ...changes.parts_removed.map((p) => `${p.filename}: removed`),
            ...changes.inputs.map(
              (c) =>
                `Source ${c.source_id}: ${fieldName(c.field)} ${valueText(c.old)} to ${valueText(c.new)}`,
            ),
          ]}
        />
      </Section>
      <Section title="Printed units affected">
        <List
          none="No printed units are affected."
          items={units.printed_affected.map(
            (u) =>
              `${u.filename}, unit ${u.unit_index + 1}: printed${u.assembled ? " and assembled" : ""}`,
          )}
        />
        {(units.added.length > 0 || units.removed.length > 0) && (
          <p>
            {plural(units.added.length, "unit")} added, {plural(units.removed.length, "unit")}{" "}
            removed.
          </p>
        )}
      </Section>
      <Section title="Plates">
        <List
          none="No plates change."
          items={changes.plates.cleared.map(
            (p) =>
              `Plate ${p.ordinal} on ${p.printer_name} is cleared (${plural(p.unit_count, "unit")})`,
          )}
        />
        {changes.plates.cleared.length > 0 && <p>{PLATES_CLEARED}</p>}
      </Section>
      <Section title="Stock">
        {changes.stock.kind === "off" ? (
          <p className="pp-muted">Spoolman is off, so stock impact is not checked.</p>
        ) : (
          <List
            none="No spool changes."
            items={changes.stock.spools.map(
              (s) =>
                `Spool ${s.spool_id}: ${s.unprinted_before} unprinted units to ${s.unprinted_after}`,
            )}
          />
        )}
      </Section>
      <Section title="Duplicate orders">
        <List
          none="No other Build has this order number."
          items={changes.duplicate_orders.map(
            (d) => `Order ${d.order_number} is also on ${d.name}`,
          )}
        />
      </Section>
    </>
  );
}

/** Why Accept is off, or null when it can run. */
function acceptBlocked(preview: DraftAcceptPreview | undefined, offline: boolean): string | null {
  if (offline) return OFFLINE_REASON;
  if (!preview) return "The preview has not loaded yet.";
  if (preview.kind !== "changed") return null;
  const { blocked_by, production } = preview.changes;
  if (blocked_by.length > 0) {
    const kinds = blocked_by.map((b) => b.kind.replaceAll("_", " "));
    return `Accept waits for running work to finish: ${kinds.join(", ")}.`;
  }
  if (production) {
    const links = plural(production.checkoff_link_count, "checkoff link");
    const sends = plural(production.send_queue_item_count, "queued send");
    return `Accept is off while ${links} and ${sends} point at this Plan. Finish or clear them first.`;
  }
  return null;
}

/** Why the gate's Accept is off, or null when it can run. */
function gateBlocked({
  preview,
  data,
  needsMatch,
  offline,
  accepting,
}: {
  preview: { isError: boolean; isFetching: boolean };
  data: DraftAcceptPreview | undefined;
  needsMatch: boolean;
  offline: boolean;
  accepting: boolean;
}): string | null {
  if (!offline && needsMatch) return "Match the printed units first.";
  if (!offline && preview.isError) return "The preview failed. Check again before you accept.";
  const reason = acceptBlocked(data, offline);
  if (reason) return reason;
  if (accepting) return "Accepting.";
  return preview.isFetching ? "Checking what Accept would change." : null;
}

/**
 * The Accept gate: previews old against new through Accept's own checks, says that a
 * snapshot is saved first, and accepts exactly the revision it previewed.
 */
function AcceptGate({
  id,
  workspace,
  restoredFrom,
  onClose,
  onAccepted,
}: {
  id: number;
  workspace: DraftWorkspace;
  /** The snapshot this draft restores, when History opened the gate. */
  restoredFrom: string | null;
  onClose: () => void;
  onAccepted: (receipt: AcceptDraftReceipt) => void;
}) {
  const offline = useConnection() === "offline";
  const draftId = workspace.draft.draft_id;
  const [key, setKey] = useState(newKey);
  const reasonId = useId();
  const preview = useQuery({
    queryKey: ["core", "accept-preview", id, draftId],
    queryFn: ({ signal }) => readCore("previewDraftAccept", { id, draft_id: draftId }, signal),
    staleTime: 0,
    gcTime: 0,
    retry: false,
  });
  const accept = useMutation({
    mutationFn: (previewed: DraftAcceptPreview) =>
      writeCore("acceptDraft", { id, draft_id: draftId }, acceptBody(previewed), key),
    retry: (failures: number, error: Error) => failures < 2 && resendable(error),
    onSuccess: onAccepted,
    onError: (error) => {
      // A refusal ends this key; the next Accept is a new request against a new preview. An
      // unknown outcome keeps it, so the next Accept replays the first receipt if one exists.
      if (!resendable(error)) {
        setKey(newKey());
        void preview.refetch();
      }
    },
  });
  // A failed re-preview keeps the old data; that preview is no longer what Accept checks.
  const data = preview.isError ? undefined : preview.data;
  // A match clears the last Accept refusal; the next Accept answers against a new preview.
  const match = useMatchStep(id, workspace, preview, () => accept.reset());
  const blocked = gateBlocked({
    preview,
    data,
    needsMatch: match.needed,
    offline,
    accepting: accept.isPending,
  });
  return (
    <Dialog title="Accept this draft" onClose={onClose} className="pp-gate">
      {restoredFrom && (
        <p className="pp-gate-restored">
          This draft restores {restoredFrom}. Accepting publishes that Plan. Printed units and
          printer commands stay as they are.
        </p>
      )}
      {preview.isPending && <p role="status">Checking what Accept would change.</p>}
      {match.step}
      {preview.isError && !match.needed && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{refusalText(preview.error)}</p>
          <button type="button" className="pp-button" onClick={() => void preview.refetch()}>
            Check again
          </button>
        </div>
      )}
      {data && (
        <>
          {match.done}
          <p className="pp-gate-snapshot">{data.snapshot.detail}</p>
          {data.kind === "no_change" ? (
            <p>Accepting changes nothing in the Plan.</p>
          ) : (
            <Changes changes={data.changes} />
          )}
        </>
      )}
      {accept.isError && (
        <p role="alert" className="pp-field-error">
          {refusalText(accept.error, "accept")}
        </p>
      )}
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="pp-button pp-button-primary"
          aria-disabled={blocked ? true : undefined}
          aria-describedby={blocked ? reasonId : undefined}
          onClick={() => !blocked && data && accept.mutate(data)}
        >
          Accept
        </button>
        {blocked && (
          <span id={reasonId} className="pp-mutation-reason">
            {blocked}
          </span>
        )}
      </div>
    </Dialog>
  );
}

const refreshAfterAccept = (client: QueryClient, id: number) =>
  Promise.all(
    [planKey(id), ["core", "build", id], ["core", "workflow", id]].map((queryKey) =>
      client.invalidateQueries({ queryKey }),
    ),
  );

function PlanBody({ id, data }: { id: number; data: PlanData }) {
  const client = useQueryClient();
  const writes = useDraftWrites(id);
  const draft = data.draft;
  const location = useLocation();
  const navigate = useNavigate();
  // History's Restore as draft lands here with the restored draft, whose gate opens at once.
  // The state is cleared so a reload or Back does not open it again.
  const [restored] = useState(() => {
    const state = restoredOf(location.state);
    return state && state.draftId === draft?.workspace.draft.draft_id ? state : null;
  });
  const [gate, setGate] = useState(restored !== null);
  useEffect(() => {
    if (restoredOf(location.state)) {
      void navigate(location.pathname, { replace: true, state: null });
    }
  }, [location.state, location.pathname, navigate]);
  const [accepted, setAccepted] = useState<AcceptDraftReceipt | null>(null);
  // A field's blur can start a save in the same turn as a click, so ask the cache, not
  // the last render, whether one is still running.
  const saving = () => client.isMutating({ mutationKey: draftWriteKey(id) }) > 0;
  const busy = useIsMutating({ mutationKey: draftWriteKey(id) }) > 0;
  return (
    <>
      <div className="pp-plan-status">
        <p>{draft ? DRAFT_OPEN : NO_DRAFT}</p>
        {accepted && (
          <p role="status">
            Accepted. The Plan is now version <span className="num">{accepted.plan_version}</span>.
          </p>
        )}
        {draft ? (
          <MutationButton primary onClick={() => setGate(true)}>
            Review and accept
          </MutationButton>
        ) : (
          <MutationButton
            onClick={() =>
              !saving() && writes.header.mutate({ fields: fieldsOf(data.build), key: newKey() })
            }
          >
            Start a draft
          </MutationButton>
        )}
      </div>
      <SaveStatus writes={writes} />
      <HeaderForm key={id} data={data} writes={writes} busy={busy} />
      {draft && <PartsTable parts={draft.workspace.parts} writes={writes} />}
      {gate && draft && !busy && (
        <AcceptGate
          id={id}
          workspace={draft.workspace}
          restoredFrom={
            restored?.draftId === draft.workspace.draft.draft_id ? restored.summary : null
          }
          onClose={() => setGate(false)}
          onAccepted={(receipt) => {
            setGate(false);
            writes.header.reset();
            writes.parts.reset();
            setAccepted(receipt);
            void refreshAfterAccept(client, id);
          }}
        />
      )}
    </>
  );
}

/** The Plan tab: the draft's header and Parts, saved as they change, and the Accept gate. */
export function BuildPlan() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const query = useQuery(planQuery(id));
  if (query.error instanceof CoreError && query.error.status === 404 && !query.data) {
    return <NotFound />;
  }
  return (
    <ScreenTemplate
      title="Plan"
      subnav={<BuildTabs id={id} />}
      state={queryScreenState(query, connection)}
      renderData={(data) => <PlanBody id={id} data={data} />}
      empty={{ message: "" }}
    />
  );
}
