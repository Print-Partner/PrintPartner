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
import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
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
import { ExperimentOption, holdersOf } from "./ExperimentOption";
import { planKey, useMatchStep } from "./PlanReconcile";
import {
  readPlanDraft,
  RefreshNotice,
  SOURCE_CHANGED,
  staleInputs,
  usePendingEdits,
  usePlanRefresh,
  type OpenPlan,
  type PendingEdits,
} from "./PlanRefresh";
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
export const planQuery = (id: number, selected?: RefObject<number | null>) => ({
  queryKey: planKey(id),
  queryFn: async ({ signal }: { signal: AbortSignal }): Promise<PlanData> => {
    const [drafts, { build, base }] = await Promise.all([
      readCore("listDrafts", { id }, signal),
      readCore("readBuildHeader", { id }, signal),
    ]);
    const open = drafts.drafts.find(
      (d) => d.state === "open" && (selected?.current == null || d.draft_id === selected.current),
    );
    if (!open) return { build, base, draft: null };
    const loaded = await readPlanDraft(id, open.draft_id, signal);
    return loaded.draft.workspace.draft.state === "open"
      ? { ...loaded, base }
      : { build, base, draft: null };
  },
  retry: (failures: number, error: Error) =>
    !(error instanceof CoreError && error.status === 404) && failures < 3,
});

/** Draft writes run one at a time, each against the draft the last one left. */
function useDraftWrites(
  id: number,
  pinDraft: PinDraft,
  recovery: RefObject<OpenPlan | null>,
  markStale: (data: OpenPlan) => void,
) {
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
    if (!recovery.current) return client.invalidateQueries({ queryKey: planKey(id) });
  };
  const observed = async <T,>(data: PlanData, save: () => Promise<T>) => {
    try {
      return await save();
    } catch (error) {
      if (staleInputs(error) && data.draft && !recovery.current) {
        markStale({ ...data, draft: data.draft });
      }
      throw error;
    }
  };
  const retry = (failures: number, error: Error) =>
    !recovery.current && failures < 2 && resendable(error);
  const scope = { id: `plan-draft-${id}` };
  const mutationKey = draftWriteKey(id);
  const header = useMutation({
    scope,
    mutationKey,
    // A 401 keeps the page, so the typed edit stays on screen (`SignedOutNote`).
    meta: KEEP_EDITS,
    // `changes` holds only the fields typed for this write. The rest come from the header
    // saved when the write starts, so a queued write never resends an older field value.
    mutationFn: ({ changes, key }: { changes: Partial<HeaderFields>; key: string }) => {
      if (recovery.current) throw new Error(SOURCE_CHANGED);
      const { body, data } = once(key, () => {
        const data = latest();
        if (!data) throw new Error("The Plan has not loaded yet");
        const saved = data.draft?.header?.header ?? data.build;
        return {
          data,
          body: {
            expected_base: data.draft?.workspace.draft.base ?? data.base,
            expected_draft: data.draft ? identityOf(data.draft.workspace.draft) : null,
            expected_header_version: data.draft?.header?.header_version ?? null,
            header: headerOf({ ...fieldsOf(saved), ...changes }),
          },
        };
      });
      return observed(data, () => writeCore("saveDraftHeader", { id }, body, key));
    },
    retry,
    onSuccess: ({ header }) => pinDraft(header.draft_id, true),
    onSettled: (_data, _error, { key }) => settle(key),
  });
  const parts = useMutation({
    scope,
    mutationKey,
    meta: KEEP_EDITS,
    mutationFn: ({ choice, key }: { choice: PlanChoice; key: string }) => {
      if (recovery.current) throw new Error(SOURCE_CHANGED);
      const { body, data } = once(key, () => {
        const data = latest();
        const draft = data?.draft;
        if (!draft) throw new Error("No draft is open");
        return {
          data,
          body: {
            decisions: [choice],
            expected_base: draft.workspace.draft.base,
            expected_draft: identityOf(draft.workspace.draft),
            remap_checkoff_links: false,
          },
        };
      });
      return observed(data, () => writeCore("autosaveDraft", { id }, body, key));
    },
    retry,
    // The next write in the queue names the draft this one left.
    onSuccess: ({ workspace }) =>
      client.setQueryData<PlanData>(planKey(id), (data) =>
        data?.draft?.workspace.draft.draft_id === workspace.draft.draft_id
          ? { ...data, draft: { ...data.draft, workspace } }
          : data,
      ),
    onSettled: (_data, _error, { key }) => settle(key),
  });
  return { header, parts, mutationKey };
}

type Writes = ReturnType<typeof useDraftWrites>;

const keyOf = (mutation: { state: { variables: unknown } }) =>
  (mutation.state.variables as { key?: string } | undefined)?.key;

/**
 * The draft writes sent with `keys` that have settled, and whether each failed: refused,
 * or its outcome never confirmed. A field that sent one keeps its value only when it failed.
 */
function useSettledWrites(writes: Writes, keys: readonly string[]) {
  return useMutationState({
    filters: {
      mutationKey: writes.mutationKey,
      predicate: (mutation) =>
        keys.includes(keyOf(mutation) ?? "") &&
        (mutation.state.status === "success" || mutation.state.status === "error"),
    },
    select: (mutation) => ({
      key: keyOf(mutation) ?? "",
      failed: mutation.state.status === "error",
    }),
  });
}

/** What the last write did, read out politely. */
function SaveStatus({ writes }: { writes: Writes }) {
  // Writes run one at a time, so the last submitted is the last settled.
  const last = writes.header.submittedAt >= writes.parts.submittedAt ? writes.header : writes.parts;
  const failed = last.error;
  const text =
    writes.header.isPending || writes.parts.isPending
      ? "Saving to the draft"
      : failed
        ? staleInputs(failed) || failed.message === SOURCE_CHANGED
          ? "Not saved. Your unsaved values are kept in this workspace."
          : `${resendable(failed) ? "Save not confirmed" : "Not saved"}: ${refusalText(failed, "save")}`
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

/** A header write not yet settled, and the values typed for it. */
interface HeldWrite {
  key: string;
  changes: Partial<HeaderFields>;
}

const carries = (write: HeldWrite, field: keyof HeaderFields) => field in write.changes;

/**
 * The header form. Each field saves to the draft when it loses focus or on Enter, and each
 * write carries only the fields typed since the last one. Once the draft writes settle, the
 * saved header replaces every field not typed since. A field whose write fails, even because
 * another window saved the same field, counts as typed again: it keeps its value and the
 * next blur sends it. A blur with nothing typed sends nothing.
 */
function HeaderForm({
  data,
  writes,
  busy,
  locked,
  pending,
}: {
  data: PlanData;
  writes: Writes;
  busy: boolean;
  locked: boolean;
  pending: PendingEdits;
}) {
  const saved = data.draft?.header?.header ?? data.build;
  const [fields, setFields] = useState(() => fieldsOf(saved));
  // The header the fields last took, or null after a write settles, so they take it again.
  const [shown, setShown] = useState<BuildHeader | null>(saved);
  const [typed, setTyped] = useState<ReadonlySet<keyof HeaderFields>>(new Set());
  // Header writes not yet settled, oldest first.
  const [held, setHeld] = useState<readonly HeldWrite[]>([]);
  const settled = useSettledWrites(
    writes,
    held.map((write) => write.key),
  );
  let kept = typed;
  if (settled.length > 0) {
    const failed = new Set(settled.filter((write) => write.failed).map((write) => write.key));
    const done = new Set(settled.map((write) => write.key));
    // A failed write's field counts as typed again, unless a later write carries it.
    const restored = held.flatMap((write, i) =>
      failed.has(write.key)
        ? (Object.keys(write.changes) as (keyof HeaderFields)[]).filter(
            (f) => !held.slice(i + 1).some((later) => carries(later, f)),
          )
        : [],
    );
    kept = new Set([...typed, ...restored]);
    setHeld(held.filter((write) => !done.has(write.key)));
    setTyped(kept);
  }
  if (settled.length > 0 || shown === null || !sameHeader(saved, shown)) {
    if (busy) {
      if (shown !== null) setShown(null);
    } else {
      const after = fieldsOf(saved);
      setShown(saved);
      setFields(
        Object.fromEntries(
          Object.entries(fields).map(([field, v]) => [
            field,
            kept.has(field as keyof HeaderFields) ? v : after[field as keyof HeaderFields],
          ]),
        ) as HeaderFields,
      );
    }
  }
  const [invalid, setInvalid] = useState(false);
  const offline = useConnection() === "offline";
  const orders = useModule("orders");
  const ids = { form: useId(), reason: useId(), name: useId() };
  usePendingEdits(
    pending,
    ids.form,
    HEADER_FIELDS.filter(({ field }) => kept.has(field)).map(
      ({ field, label }) => `${label}: ${fields[field] || "(blank)"}`,
    ),
  );
  const commit = () => {
    const header = headerOf(fields);
    setInvalid(header.name === "");
    if (header.name === "" || offline || locked || typed.size === 0) return;
    // A field is sent only when it differs from the value it will hold without this write:
    // the latest unsettled write's value for it, or else the saved value.
    const base = fieldsOf(saved);
    const value = (field: keyof HeaderFields, text: string) =>
      headerOf({ ...base, [field]: text })[field];
    const changes: Partial<HeaderFields> = Object.fromEntries(
      [...typed]
        .filter((field) => {
          const pending = held.findLast((write) => carries(write, field))?.changes[field];
          return value(field, fields[field]) !== value(field, pending ?? base[field]);
        })
        .map((field) => [field, fields[field]]),
    );
    setTyped(new Set());
    if (Object.keys(changes).length === 0) return;
    const key = newKey();
    setHeld([...held, { key, changes }]);
    writes.header.mutate({ changes, key });
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
              readOnly={offline || locked}
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
 * A Part's quantity field. It shows the saved quantity, except text typed since the last
 * save was sent, or sent by a save that failed, even because another window saved this
 * Part: the field keeps that text, and the next blur or Enter sends it.
 */
function useQuantityField(
  part: PartView,
  writes: Writes,
  queued: PlanChoice | undefined,
  locked: boolean,
  save: (choice: PlanChoice) => string,
) {
  const [text, setText] = useState(part.quantity_override?.toString() ?? "");
  // The quantity the text last took, or undefined after a write settles, so it takes it again.
  const [shown, setShown] = useState<number | null | undefined>(part.quantity_override);
  const [edited, setEdited] = useState(false);
  const [sentKey, setSentKey] = useState<string | null>(null);
  const [settled] = useSettledWrites(writes, sentKey === null ? [] : [sentKey]);
  if (settled) {
    setSentKey(null);
    if (settled.failed) setEdited(true);
  }
  if (settled || shown === undefined || part.quantity_override !== shown) {
    if (queued) {
      if (shown !== undefined) setShown(undefined);
    } else {
      setShown(part.quantity_override);
      if (!edited && !settled?.failed) setText(part.quantity_override?.toString() ?? "");
    }
  }
  const [invalid, setInvalid] = useState(false);
  const sent = queued?.kind === "set_quantity_override" ? queued.value : part.quantity_override;
  return {
    text,
    invalid,
    pending: edited || settled?.failed,
    type: (value: string) => {
      setText(value);
      setEdited(true);
    },
    commit: () => {
      const value = quantityValue(text);
      setInvalid(value === "invalid");
      // A blur with nothing typed since the last write sends nothing.
      if (value === "invalid" || locked || !edited) return;
      setEdited(false);
      if (value === sent) return;
      setSentKey(save(quantityChoice(part, value)));
    },
  };
}

/**
 * One Part row: whether it is included and its quantity override. While an Included save
 * is queued or running, the box shows its value, and the next click toggles from that.
 */
function useIncludedField(
  part: PartView,
  writes: Writes,
  queued: PlanChoice | undefined,
  save: (choice: PlanChoice) => string,
) {
  const included = queued?.kind === "set_included" ? queued.value : part.included;
  const [last, setLast] = useState<{ key: string; value: boolean } | null>(null);
  const [unsaved, setUnsaved] = useState<string | null>(null);
  const [settled] = useSettledWrites(writes, last ? [last.key] : []);
  if (last && settled) {
    setLast(null);
    setUnsaved(settled.failed ? (last.value ? "Include" : "Exclude") : null);
  }
  return {
    included,
    unsaved,
    toggle: () => setLast({ key: save(includeChoice(part, !included)), value: !included }),
  };
}

function PartRow({
  part,
  writes,
  reasonId,
  locked,
  edits,
}: {
  part: PartView;
  writes: Writes;
  reasonId: string;
  locked: boolean;
  edits: PendingEdits;
}) {
  const offline = useConnection() === "offline";
  const target = JSON.stringify(includeChoice(part, true).target);
  const pending = useMutationState({
    filters: { mutationKey: writes.mutationKey, status: "pending" },
    select: (mutation) => (mutation.state.variables as { choice?: PlanChoice } | undefined)?.choice,
  }).filter((choice) => choice && JSON.stringify(choice.target) === target);
  const queued = (kind: PlanChoice["kind"]) => pending.findLast((choice) => choice?.kind === kind);
  const errorId = useId();
  const save = (choice: PlanChoice) => {
    const key = newKey();
    writes.parts.mutate({ choice, key });
    return key;
  };
  const quantity = useQuantityField(
    part,
    writes,
    queued("set_quantity_override"),
    locked || offline,
    save,
  );
  const included = useIncludedField(part, writes, queued("set_included"), save);
  usePendingEdits(edits, errorId, [
    ...(quantity.pending
      ? [`${part.relative_path} quantity: ${quantity.text || "(from Source)"}`]
      : []),
    ...(included.unsaved ? [`${part.relative_path}: ${included.unsaved}`] : []),
  ]);
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
          checked={included.included}
          aria-label={`Include ${part.filename}`}
          aria-disabled={offline || locked || undefined}
          aria-describedby={offline ? reasonId : undefined}
          onChange={() => {
            if (offline || locked) return;
            included.toggle();
          }}
        />
        {locked && included.unsaved && (
          <span className="pp-field-error">Unsaved: {included.unsaved}</span>
        )}
      </td>
      <td>
        <input
          className="pp-quantity num"
          inputMode="numeric"
          value={quantity.text}
          placeholder={String(part.quantity_inferred)}
          readOnly={offline || locked}
          aria-label={`Quantity of ${part.filename}, blank for ${part.quantity_inferred} from the Source`}
          aria-invalid={quantity.invalid || undefined}
          aria-describedby={
            [quantity.invalid && errorId, offline && reasonId].filter(Boolean).join(" ") ||
            undefined
          }
          onChange={(event) => quantity.type(event.target.value)}
          onBlur={quantity.commit}
          onKeyDown={(event) => event.key === "Enter" && quantity.commit()}
        />
        {quantity.invalid && (
          <span id={errorId} className="pp-field-error">
            A whole number from 1 to 10,000, or blank.
          </span>
        )}
      </td>
    </tr>
  );
}

function PartsTable({
  parts,
  writes,
  locked,
  edits,
}: {
  parts: PartView[];
  writes: Writes;
  locked: boolean;
  edits: PendingEdits;
}) {
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
            <PartRow
              key={part.draft_part_id}
              part={part}
              writes={writes}
              reasonId={reasonId}
              locked={locked}
              edits={edits}
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
        {changes.duplicate_orders.length > 0 && (
          <ExperimentOption
            holders={changes.duplicate_orders.map((d) => ({
              profileId: d.profile_id,
              name: d.name,
            }))}
          />
        )}
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
      {preview.isError && !match.needed && holdersOf(preview.error).length > 0 && (
        <ExperimentOption holders={holdersOf(preview.error)} />
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

type PinDraft = (id: number, onlyIfPinned?: boolean) => void;

function UnsavedEdits({ edits, onDismiss }: { edits: string[]; onDismiss: () => void }) {
  if (!edits.length) return null;
  return (
    <section className="pp-plan-refresh" aria-label="Unsaved edits">
      <p>
        Unsaved edits from the previous workspace. Review the refreshed Parts and re-enter any
        values you still need.
      </p>
      <ul>
        {edits.map((edit) => (
          <li key={edit}>{edit}</li>
        ))}
      </ul>
      <button className="pp-button" type="button" onClick={onDismiss}>
        Dismiss unsaved edits
      </button>
    </section>
  );
}

function useInstalledPlan(queryData: PlanData) {
  const [installed, setInstalled] = useState<OpenPlan | null>(null);
  if (
    installed &&
    installed.draft.workspace.draft.draft_id === queryData.draft?.workspace.draft.draft_id
  ) {
    setInstalled(null);
    return [queryData, setInstalled] as const;
  }
  return [installed ?? queryData, setInstalled] as const;
}

function PlanBody({
  id,
  queryData,
  pinDraft,
}: {
  id: number;
  queryData: PlanData;
  pinDraft: PinDraft;
}) {
  const client = useQueryClient();
  const sourceRef = useRef<OpenPlan | null>(null);
  const [stale, setStale] = useState<OpenPlan | null>(null);
  const [current, setInstalled] = useInstalledPlan(queryData);
  const data = stale ?? current;
  const draft = data.draft;
  const draftId = draft?.workspace.draft.draft_id;
  const location = useLocation();
  const navigate = useNavigate();
  // History's Restore as draft lands here with the restored draft, whose gate opens at once.
  // The state is cleared so a reload or Back does not open it again.
  const [restored] = useState(() => {
    const state = restoredOf(location.state);
    return state && state.draftId === draftId ? state : null;
  });
  const [gate, setGate] = useState(restored !== null);
  const writes = useDraftWrites(id, pinDraft, sourceRef, (source) => {
    sourceRef.current = source;
    setStale(source);
    setGate(false);
  });
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
  const edits = useRef<PendingEdits>(new Map());
  const [unsaved, setUnsaved] = useState<string[]>([]);
  const [refreshed, setRefreshed] = useState<number | null>(null);
  const refresh = usePlanRefresh(id, sourceRef, saving, KEEP_EDITS, async (plan) => {
    await client.cancelQueries({ queryKey: planKey(id), exact: true });
    const kept = [...edits.current.values()].flat();
    setUnsaved((previous) => [...new Set([...previous, ...kept])]);
    const successor = plan.draft.workspace.draft.draft_id;
    pinDraft(successor);
    client.setQueryData(planKey(id), plan);
    sourceRef.current = null;
    setInstalled(plan);
    setStale(null);
    setRefreshed(successor);
    writes.header.reset();
    writes.parts.reset();
  });
  const editorKey = refreshed ?? "original";
  return (
    <>
      <div className="pp-plan-status">
        <p>{draft ? DRAFT_OPEN : NO_DRAFT}</p>
        {accepted && (
          <p role="status">
            Accepted. The Plan is now version <span className="num">{accepted.plan_version}</span>.
          </p>
        )}
        {stale ? null : draft ? (
          <MutationButton primary onClick={() => !sourceRef.current && setGate(true)}>
            Review and accept
          </MutationButton>
        ) : (
          <MutationButton
            onClick={() => !saving() && writes.header.mutate({ changes: {}, key: newKey() })}
          >
            Start a draft
          </MutationButton>
        )}
      </div>
      {stale ? (
        <RefreshNotice
          refresh={refresh}
          saving={busy}
          signedOutNote={<RefreshSignedOut error={refresh.error} />}
        />
      ) : refreshed === draftId ? (
        <p role="status">Draft refreshed from current Sources.</p>
      ) : null}
      <UnsavedEdits edits={unsaved} onDismiss={() => setUnsaved([])} />
      <SaveStatus writes={writes} />
      <HeaderForm
        key={`header-${editorKey}`}
        data={data}
        writes={writes}
        busy={busy}
        locked={stale !== null}
        pending={edits.current}
      />
      {draft && (
        <PartsTable
          key={`parts-${editorKey}`}
          parts={draft.workspace.parts}
          writes={writes}
          locked={stale !== null}
          edits={edits.current}
        />
      )}
      {gate && draft && !busy && !stale && (
        <AcceptGate
          id={id}
          workspace={draft.workspace}
          restoredFrom={restored && restored.draftId === draftId ? restored.summary : null}
          onClose={() => setGate(false)}
          onAccepted={(receipt) => {
            setGate(false);
            writes.header.reset();
            writes.parts.reset();
            setAccepted(receipt);
            setRefreshed(null);
            void refreshAfterAccept(client, id);
          }}
        />
      )}
    </>
  );
}

function RefreshSignedOut({ error }: { error: Error | null }) {
  return error && signedOut(error) ? <SignedOutNote /> : null;
}

/** The Plan tab: the draft's header and Parts, saved as they change, and the Accept gate. */
export function BuildPlan() {
  const { id } = useOutletContext<WorkspaceContext>();
  return <BuildPlanScreen key={id} id={id} />;
}

function BuildPlanScreen({ id }: { id: number }) {
  const connection = useConnection();
  const selected = useRef<number | null>(null);
  const pinDraft: PinDraft = (draftId, onlyIfPinned) => {
    if (!onlyIfPinned || selected.current !== null) selected.current = draftId;
  };
  const query = useQuery(planQuery(id, selected));
  if (query.error instanceof CoreError && query.error.status === 404 && !query.data) {
    return <NotFound />;
  }
  return (
    <ScreenTemplate
      title="Plan"
      subnav={<BuildTabs id={id} />}
      state={queryScreenState(query, connection)}
      renderData={(data) => <PlanBody id={id} queryData={data} pinDraft={pinDraft} />}
      empty={{ message: "" }}
    />
  );
}
