import type { DraftWorkspace } from "@print-partner/rust-client";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { newKey, resendable } from "../core/writes";
import { useConnection } from "../frame/connection";
import { plural } from "../today/beds";
import {
  conflictOptions,
  conflictQuestion,
  refusalText,
  unitConflicts,
  type PlanData,
  type UnitConflict,
  type UnitDecision,
} from "./plan";

export const planKey = (id: number) => ["core", "plan", id] as const;

/**
 * Saves and selects the draft's Required-unit reconciliation: the core's automatic one
 * (no decisions), or the user's decisions. It shares the draft writes' scope, so it runs
 * after any save still queued, and its body names the snapshot the last write left. A
 * lost send is resent with the same key and body; so is the next send of the same
 * decisions after an unconfirmed outcome, and the core replays the first answer.
 */
export function useReconcile(id: number, draftId: number) {
  const client = useQueryClient();
  // The key and body of a send whose outcome is not known yet.
  const unsure = useRef<{ key: string; decisions: string; digest: string } | null>(null);
  const mutation = useMutation({
    scope: { id: `plan-draft-${id}` },
    mutationKey: ["core", "plan-reconcile", id],
    mutationFn: ({
      decisions,
      key,
      digest,
    }: {
      decisions: UnitDecision[];
      key: string;
      digest: string;
    }) =>
      writeCore(
        "reconcileDraft",
        { id, draft_id: draftId },
        { decisions, expected_snapshot_digest: digest },
        key,
      ),
    retry: (failures: number, error: Error) => failures < 2 && resendable(error),
    // The selection changes the draft's snapshot digest; later writes must name the new one.
    onSuccess: (workspace: DraftWorkspace) => {
      unsure.current = null;
      client.setQueryData<PlanData>(planKey(id), (data) =>
        data?.draft ? { ...data, draft: { ...data.draft, workspace } } : data,
      );
    },
    onError: (error) => {
      if (resendable(error)) return;
      unsure.current = null;
      void client.invalidateQueries({ queryKey: planKey(id) });
    },
  });
  const send = (decisions: UnitDecision[], onSaved: () => void) => {
    const text = JSON.stringify(decisions);
    const kept = unsure.current?.decisions === text ? unsure.current : null;
    const digest =
      kept?.digest ??
      client.getQueryData<PlanData>(planKey(id))?.draft?.workspace.draft.snapshot_digest;
    if (!digest) return;
    const key = kept?.key ?? newKey();
    unsure.current = { key, decisions: text, digest };
    mutation.mutate({ decisions, key, digest }, { onSuccess: onSaved });
  };
  return { mutation, send };
}

/** Why the step's send is off, or null when it can run. */
function stepBlocked({
  offline,
  saving,
  checking,
  missing,
}: {
  offline: boolean;
  saving: boolean;
  checking: boolean;
  missing: number;
}): string | null {
  if (offline) return "Offline, so nothing can be saved.";
  if (saving) return "Saving the match.";
  if (checking) return "Checking what Accept would change.";
  if (missing > 0) return `Choose for ${plural(missing, "more Part")} first.`;
  return null;
}

const predecessorOf = (decision: UnitDecision) =>
  "predecessor_revision_part_id" in decision ? decision.predecessor_revision_part_id : null;

/**
 * One conflict: its question and a radio choice per decision. Two Parts cannot keep the
 * same printed units, so a predecessor another Part was given is off here.
 */
function ConflictChoice({
  workspace,
  conflict,
  chosen,
  onChoose,
}: {
  workspace: DraftWorkspace;
  conflict: UnitConflict;
  chosen: ReadonlyMap<number, UnitDecision>;
  onChoose: (decision: UnitDecision) => void;
}) {
  const options = conflictOptions(workspace, conflict);
  const target = conflict.target_draft_part_id;
  const question = conflictQuestion(workspace, conflict);
  if (options.length === 0) return <p className="pp-reconcile-note">{question}</p>;
  const taken = new Set(
    [...chosen].filter(([t]) => t !== target).map(([, decision]) => predecessorOf(decision)),
  );
  const current = JSON.stringify(chosen.get(target));
  return (
    <fieldset className="pp-reconcile-choice">
      <legend>{question}</legend>
      {options.map(({ label, decision }, index) => {
        const predecessor = predecessorOf(decision);
        const disabled = predecessor !== null && taken.has(predecessor);
        return (
          <label key={index}>
            <input
              type="radio"
              name={`reconcile-${target}`}
              checked={current === JSON.stringify(decision)}
              disabled={disabled}
              onChange={() => onChoose(decision)}
            />
            {label}
            {disabled && " Another Part keeps these."}
          </label>
        );
      })}
    </fieldset>
  );
}

type Reconcile = ReturnType<typeof useReconcile>;

/**
 * The step the Accept gate shows when its preview answers `reconciliation_required`: the
 * printed units of the accepted Plan must be matched to this draft's Parts first. With
 * nothing for the user to decide, it sends the core's automatic match once on its own;
 * with conflicts, it asks each in plain words and sends the answers. Either way the gate
 * previews again only after the core saved the match.
 */
export function ReconcileStep({
  workspace,
  reconcile,
  autoSent,
  checking,
  onSend,
}: {
  workspace: DraftWorkspace;
  reconcile: Reconcile["mutation"];
  /** Whether the automatic match was already sent from this gate. */
  autoSent: boolean;
  /** Whether the gate is previewing again after a match. */
  checking: boolean;
  onSend: (decisions: UnitDecision[]) => void;
}) {
  const offline = useConnection() === "offline";
  const headingId = useId();
  const reasonId = useId();
  const conflicts = unitConflicts(workspace);
  const [picked, setPicked] = useState<ReadonlyMap<number, UnitDecision>>(new Map());
  // Only choices for Parts still in conflict count; the core answers each send afresh.
  const chosen = new Map(
    [...picked].filter(([t]) => conflicts.some((c) => c.target_draft_part_id === t)),
  );
  const automatic = conflicts.length === 0;
  // A draft with nothing to decide is matched without asking, once per gate. If the core
  // still asks after that, the user sends it again, so a core that keeps refusing is never
  // written to in a loop.
  const sentOnce = useRef(false);
  useEffect(() => {
    if (!automatic || autoSent || offline || sentOnce.current || reconcile.isPending) return;
    sentOnce.current = true;
    onSend([]);
  }, [automatic, autoSent, offline, reconcile.isPending, onSend]);

  const decidable = conflicts.filter((c) => conflictOptions(workspace, c).length > 0);
  const blocked = stepBlocked({
    offline,
    saving: reconcile.isPending,
    checking,
    missing: decidable.filter((c) => !chosen.has(c.target_draft_part_id)).length,
  });
  return (
    <section aria-labelledby={headingId} className="pp-gate-section pp-reconcile">
      <h3 id={headingId}>Match printed units</h3>
      <p>
        Before Accept, the units already printed for the accepted Plan are matched to this draft's
        Parts, so finished prints stay counted. Nothing on the Build changes until you accept.
      </p>
      {automatic ? (
        <>
          {reconcile.isPending && <p role="status">Matching printed units to this draft.</p>}
          {autoSent && !checking && !reconcile.isPending && !reconcile.isError && (
            <p>The core still asks for the printed units to be matched. Match them again.</p>
          )}
        </>
      ) : (
        conflicts.map((conflict) => (
          <ConflictChoice
            key={`${conflict.kind}-${conflict.target_draft_part_id}`}
            workspace={workspace}
            conflict={conflict}
            chosen={chosen}
            onChoose={(decision) =>
              setPicked(new Map(picked).set(conflict.target_draft_part_id, decision))
            }
          />
        ))
      )}
      {reconcile.isError && (
        <p role="alert" className="pp-field-error">
          {resendable(reconcile.error) ? "Match not confirmed: " : "Not matched: "}
          {refusalText(reconcile.error, "match")}
        </p>
      )}
      {/* The automatic match offers its button only once its own send has settled. */}
      {(!automatic || reconcile.isError || (autoSent && !reconcile.isPending && !checking)) && (
        <div className="pp-dialog-actions">
          <button
            type="button"
            className="pp-button"
            aria-disabled={blocked ? true : undefined}
            aria-describedby={blocked ? reasonId : undefined}
            onClick={() =>
              !blocked &&
              onSend(
                decidable
                  .map((c) => chosen.get(c.target_draft_part_id))
                  .filter((d): d is UnitDecision => d !== undefined),
              )
            }
          >
            {automatic ? "Match printed units" : "Save these choices"}
          </button>
          {blocked && (
            <span id={reasonId} className="pp-mutation-reason">
              {blocked}
            </span>
          )}
        </div>
      )}
    </section>
  );
}

/**
 * The Accept gate's match step. Accept previews only a draft whose printed units are
 * matched; until then the gate shows this step instead of the refusal, and Accept stays
 * off. After a match is saved the gate previews again, and once that preview loads,
 * focus moves to what the match did.
 */
export function useMatchStep(
  id: number,
  workspace: DraftWorkspace,
  preview: {
    error: Error | null;
    isError: boolean;
    isFetching: boolean;
    refetch: () => Promise<unknown>;
  },
): { needed: boolean; step: ReactNode; done: ReactNode } {
  const reconcile = useReconcile(id, workspace.draft.draft_id);
  const [autoSent, setAutoSent] = useState(false);
  const [matched, setMatched] = useState(false);
  const focusNext = useRef(false);
  const needed =
    preview.error instanceof CoreError && preview.error.code === "reconciliation_required";
  // The summary mounts once the new preview and the saved match are both shown.
  const focusDone = (node: HTMLParagraphElement | null) => {
    if (!node || !focusNext.current) return;
    focusNext.current = false;
    node.focus();
  };
  const send = (decisions: UnitDecision[]) => {
    if (decisions.length === 0) setAutoSent(true);
    reconcile.send(decisions, () => {
      setMatched(true);
      focusNext.current = true;
      void preview.refetch();
    });
  };
  const summary = workspace.reconciliation.kind === "ready" ? workspace.reconciliation : null;
  return {
    needed,
    step: needed && (
      <ReconcileStep
        workspace={workspace}
        reconcile={reconcile.mutation}
        autoSent={autoSent}
        checking={preview.isFetching}
        onSend={send}
      />
    ),
    done: matched && summary && (
      <p ref={focusDone} tabIndex={-1} role="status" className="pp-reconcile-done">
        Printed units matched: {plural(summary.reused_units, "unit")} kept,{" "}
        {plural(summary.new_units, "new unit")}, {plural(summary.surplus_units, "unit")} no longer
        needed.
      </p>
    ),
  };
}
