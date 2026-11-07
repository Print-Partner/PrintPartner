import type { AcceptDraftReceipt, DraftAcceptPreview } from "@print-partner/rust-client";
import {
  useIsMutating,
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from "@tanstack/react-query";
import { useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { useOutletContext } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { lostSend, newKey } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { NotFound } from "../screen/screens";
import { ModuleOff, MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { plural } from "../today/beds";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
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
  type HeaderFields,
  type PartView,
  type PlanChoice,
  type PlanData,
  type PreviewChanges,
} from "./plan";

const planKey = (id: number) => ["core", "plan", id] as const;
const draftWriteKey = (id: number) => ["core", "plan-draft-write", id] as const;

/**
 * The Build's header, the Accepted base and the open draft, if any. Every value the tab
 * shows comes from the Rust core; nothing here guesses the accepted Plan.
 */
const planQuery = (id: number) => ({
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
  const settle = () => client.invalidateQueries({ queryKey: planKey(id) });
  const scope = { id: `plan-draft-${id}` };
  const mutationKey = draftWriteKey(id);
  const header = useMutation({
    scope,
    mutationKey,
    mutationFn: ({ fields, key }: { fields: HeaderFields; key: string }) => {
      const data = latest();
      if (!data) throw new Error("The Plan has not loaded yet");
      return writeCore(
        "saveDraftHeader",
        { id },
        {
          expected_base: data.draft?.workspace.draft.base ?? data.base,
          expected_draft: data.draft ? identityOf(data.draft.workspace.draft) : null,
          expected_header_version: data.draft?.header?.header_version ?? null,
          header: headerOf(fields),
        },
        key,
      );
    },
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    onSettled: settle,
  });
  const parts = useMutation({
    scope,
    mutationKey,
    mutationFn: ({ choice, key }: { choice: PlanChoice; key: string }) => {
      const draft = latest()?.draft;
      if (!draft) throw new Error("No draft is open");
      return writeCore(
        "autosaveDraft",
        { id },
        {
          decisions: [choice],
          expected_base: draft.workspace.draft.base,
          expected_draft: identityOf(draft.workspace.draft),
          remap_checkoff_links: false,
        },
        key,
      );
    },
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    // The next write in the queue names the draft this one left.
    onSuccess: ({ workspace }) =>
      client.setQueryData<PlanData>(planKey(id), (data) =>
        data?.draft ? { ...data, draft: { ...data.draft, workspace } } : data,
      ),
    onSettled: settle,
  });
  return { header, parts };
}

type Writes = ReturnType<typeof useDraftWrites>;

/** What the last write did, read out politely. */
function SaveStatus({ writes }: { writes: Writes }) {
  const failed = writes.header.error ?? writes.parts.error;
  const text =
    writes.header.isPending || writes.parts.isPending
      ? "Saving to the draft"
      : failed
        ? `Not saved: ${refusalText(failed)}`
        : writes.header.isSuccess || writes.parts.isSuccess
          ? "Saved to the draft. The Build changes only when you accept."
          : "";
  return (
    <p role="status" className="pp-save-status" data-failed={failed ? "" : undefined}>
      {text}
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
 * The header form. Each field saves to the draft when it loses focus or on Enter. A newly
 * saved header replaces only the fields the user has not changed since the last one.
 */
function HeaderForm({ data, writes }: { data: PlanData; writes: Writes }) {
  const saved = data.draft?.header?.header ?? data.build;
  const [fields, setFields] = useState(() => fieldsOf(saved));
  const [shown, setShown] = useState(saved);
  if (!sameHeader(saved, shown)) {
    const [before, after] = [fieldsOf(shown), fieldsOf(saved)];
    setShown(saved);
    setFields(
      Object.fromEntries(
        Object.entries(fields).map(([k, v]) => {
          const field = k as keyof HeaderFields;
          return [field, v === before[field] ? after[field] : v];
        }),
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
    if (header.name === "" || sameHeader(header, saved) || offline) return;
    writes.header.mutate({ fields, key: newKey() });
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
              onChange={(event) => setFields({ ...fields, [f.field]: event.target.value })}
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

/** One Part row: whether it is included and its quantity override. */
function PartRow({ part, writes }: { part: PartView; writes: Writes }) {
  const offline = useConnection() === "offline";
  const [quantity, setQuantity] = useState(part.quantity_override?.toString() ?? "");
  const [invalid, setInvalid] = useState(false);
  const errorId = useId();
  const save = (choice: PlanChoice) => writes.parts.mutate({ choice, key: newKey() });
  const commitQuantity = () => {
    const value = quantityValue(quantity);
    setInvalid(value === "invalid");
    if (value === "invalid" || value === part.quantity_override || offline) return;
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
          checked={part.included}
          aria-label={`Include ${part.filename}`}
          aria-disabled={offline || undefined}
          onChange={() => !offline && save(includeChoice(part, !part.included))}
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
          aria-describedby={invalid ? errorId : undefined}
          onChange={(event) => setQuantity(event.target.value)}
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
  if (parts.length === 0) return <p>This draft has no Parts.</p>;
  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId}>Parts</h2>
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
            <PartRow
              key={`${part.draft_part_id}-${part.quantity_override ?? ""}`}
              part={part}
              writes={writes}
            />
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

/**
 * The Accept gate: previews old against new through Accept's own checks, says that a
 * snapshot is saved first, and accepts exactly the revision it previewed.
 */
function AcceptGate({
  id,
  draftId,
  onClose,
  onAccepted,
}: {
  id: number;
  draftId: number;
  onClose: () => void;
  onAccepted: (receipt: AcceptDraftReceipt) => void;
}) {
  const offline = useConnection() === "offline";
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
    retry: (failures: number, error: Error) => failures < 2 && lostSend(error),
    onSuccess: onAccepted,
    onError: (error) => {
      // A refusal ends this key; the next Accept is a new request against a new preview.
      if (error instanceof CoreError) {
        setKey(newKey());
        void preview.refetch();
      }
    },
  });
  const blocked = acceptBlocked(preview.data, offline) ?? (accept.isPending ? "Accepting." : null);
  const data = preview.data;
  return (
    <Dialog title="Accept this draft" onClose={onClose} className="pp-gate">
      {preview.isPending && <p role="status">Checking what Accept would change.</p>}
      {preview.isError && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>{refusalText(preview.error)}</p>
          <button type="button" className="pp-button" onClick={() => void preview.refetch()}>
            Check again
          </button>
        </div>
      )}
      {data && (
        <>
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
          {refusalText(accept.error)}
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
  const [gate, setGate] = useState(false);
  const [accepted, setAccepted] = useState<AcceptDraftReceipt | null>(null);
  const draft = data.draft;
  // A field's blur can start a save in the same turn as a click, so ask the cache, not
  // the last render, whether one is still running.
  const saving = () => client.isMutating({ mutationKey: draftWriteKey(id) }) > 0;
  const busy = useIsMutating({ mutationKey: draftWriteKey(id) }) > 0;
  return (
    <>
      <div className="pp-plan-status">
        {draft ? (
          <p>
            A draft is open. Changes save to it as you make them, and the Build changes only when
            you accept.
          </p>
        ) : (
          <p>
            No draft is open, so this shows the accepted Plan. Changing a field below starts a
            draft.
          </p>
        )}
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
      <HeaderForm data={data} writes={writes} />
      {draft && <PartsTable parts={draft.workspace.parts} writes={writes} />}
      {gate && draft && !busy && (
        <AcceptGate
          id={id}
          draftId={draft.workspace.draft.draft_id}
          onClose={() => setGate(false)}
          onAccepted={(receipt) => {
            setGate(false);
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
