import type { RefreshDraftRequest } from "@print-partner/rust-client";
import { useMutation } from "@tanstack/react-query";
import { useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { CoreError } from "../core/client";
import { readCore, RequestContractError, writeCoreAnswer } from "../core/rust";
import { newKey } from "../core/writes";
import { MutationButton } from "../screen/ScreenTemplate";
import { identityOf, type PlanData } from "./plan";

export async function readPlanDraft(
  id: number,
  draftId: number,
  signal?: AbortSignal,
  options?: { notifyUnauthorized: boolean },
) {
  const params = { id, draft_id: draftId };
  const [workspace, header] = await Promise.all([
    readCore("readDraftWorkspace", params, signal, undefined, options),
    readCore("readDraftHeader", params, signal, undefined, options),
  ]);
  if (
    workspace.profile_id !== id ||
    workspace.draft.draft_id !== draftId ||
    header.draft_id !== draftId
  ) {
    throw new Error("The core returned a different draft.");
  }
  return { build: header.build, draft: { workspace, header: header.draft } };
}

export const staleInputs = (error: unknown) =>
  error instanceof CoreError && error.code === "inputs_changed";

export const SOURCE_CHANGED = "The Sources changed since this draft was opened.";

export type OpenPlan = PlanData & { draft: NonNullable<PlanData["draft"]> };

interface Attempt {
  key: string;
  body: RefreshDraftRequest;
  successor?: number;
}

// The old workspace holds unsaved values even when a recovery read fails with 401.
const RECOVERY_READ = { notifyUnauthorized: false };

const REFUSALS: Record<string, string> = {
  base_changed: "The Accepted Plan changed. Keep any unsaved values before opening another draft.",
  competing_draft: "Another draft is open for this Build. Resolve it before trying again.",
  draft_changed: "This draft changed elsewhere. Check the draft before trying again.",
  draft_not_author: "Only the draft’s author can refresh it.",
  draft_not_found: "The core could not find this draft.",
  header_changed: "The draft header changed elsewhere. Check the draft before trying again.",
  inputs_changed: "The Sources changed again during refresh. Try again when they are stable.",
  inputs_unchanged: "The core sees the same Source inputs as when this draft opened.",
  merge_conflicts:
    "Some saved Part choices could not be carried to the current Sources. Check the Parts in the Sources before trying again.",
  not_open: "This draft is no longer open.",
  unsupported_draft_decisions: "This draft contains saved decisions that refresh cannot carry.",
  untracked_inputs: "The core is missing a tracked revision for one or more Sources.",
  would_wipe: "The current Sources would leave the draft with no Parts.",
};

export function usePlanRefresh(
  id: number,
  sourceRef: RefObject<OpenPlan | null>,
  saving: () => boolean,
  meta: Record<string, unknown>,
  onRefreshed: (plan: OpenPlan) => Promise<void>,
) {
  const attempt = useRef<Attempt | null>(null);
  const running = useRef(false);
  const [checkStatus, setCheckStatus] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [checkError, setCheckError] = useState<Error | null>(null);
  const refresh = useMutation({
    networkMode: "always",
    retry: false,
    meta,
    mutationFn: async (source: OpenPlan) => {
      const draft = source.draft;
      const draftId = draft.workspace.draft.draft_id;
      const checking = attempt.current !== null;
      const current = (attempt.current ??= {
        key: newKey(),
        body: {
          expected_draft: identityOf(draft.workspace.draft),
          expected_header_version: draft.header?.header_version ?? null,
        },
      });
      if (current.successor === undefined) {
        try {
          const { status, data } = await writeCoreAnswer(
            "refreshDraft",
            { id, draft_id: current.body.expected_draft.draft_id },
            { redirect: "error" },
            current.body,
            current.key,
          );
          if (
            status !== 200 ||
            data.source_draft_id !== draftId ||
            data.successor_draft_id === draftId
          ) {
            throw new Error("The core did not confirm the successor.");
          }
          current.successor = data.successor_draft_id;
        } catch (error) {
          if (
            error instanceof RequestContractError ||
            (error instanceof CoreError &&
              error.problem &&
              !error.redirected &&
              error.status < 500 &&
              error.code !== "outcome_unknown" &&
              // Authorization can fail before replaying a committed attempt.
              !(checking && (error.status === 401 || error.status === 403)))
          ) {
            attempt.current = null;
          }
          throw error;
        }
      }
      const target = await readPlanDraft(id, current.successor, undefined, RECOVERY_READ);
      if (target.draft.workspace.draft.state !== "open") {
        throw new Error("The refreshed draft is no longer open.");
      }
      return { ...source, ...target };
    },
    onSuccess: async (plan) => {
      await onRefreshed(plan);
      attempt.current = null;
    },
  });
  const error = refresh.error;
  const code = error instanceof CoreError ? error.code : undefined;
  const checkDraft = !attempt.current && (code === "draft_changed" || code === "header_changed");
  const recover = async () => {
    const source = sourceRef.current;
    if (!source || running.current || saving()) return;
    running.current = true;
    setCheckStatus(null);
    setCheckError(null);
    try {
      if (!checkDraft) {
        await refresh.mutateAsync(source);
        return;
      }
      setChecking(true);
      const current = await readPlanDraft(
        id,
        source.draft.workspace.draft.draft_id,
        undefined,
        RECOVERY_READ,
      );
      if (current.draft.workspace.draft.state !== "open") {
        throw new Error("This draft is no longer open.");
      }
      sourceRef.current = { ...source, ...current };
      refresh.reset();
      setCheckStatus("Draft checked. Refresh will use its latest saved edits.");
    } catch (error) {
      if (checkDraft) {
        if (error instanceof Error) setCheckError(error);
        setCheckStatus(
          "The current draft could not be checked. Your unsaved values are still available below.",
        );
      }
    } finally {
      running.current = false;
      setChecking(false);
    }
  };
  return {
    error: checkError ?? error,
    checkStatus,
    busy: checking || refresh.isPending,
    recover,
    action: checkDraft
      ? "Check current draft"
      : attempt.current?.successor !== undefined
        ? "Load refreshed draft"
        : attempt.current
          ? "Check refresh"
          : "Refresh from Sources",
    message: refresh.isPending
      ? "Checking the draft against the current Sources…"
      : error && attempt.current?.successor !== undefined
        ? "The core confirmed refresh, but the refreshed draft could not be opened. Your previous workspace is shown below."
        : error && attempt.current
          ? "The core could not confirm refresh. Check again to recover the result of the same request."
          : error
            ? `Refresh refused. ${(code && REFUSALS[code]) || "The current draft is still shown below."}`
            : null,
  };
}

export function RefreshNotice({
  refresh,
  saving,
  signedOutNote,
}: {
  refresh: ReturnType<typeof usePlanRefresh>;
  saving: boolean;
  signedOutNote: ReactNode;
}) {
  return (
    <section className="pp-plan-refresh" aria-label="Source changes">
      <p role="status">
        {SOURCE_CHANGED} Editing and Accept are paused until the draft is refreshed.
      </p>
      {refresh.message && <p role="status">{refresh.message}</p>}
      {refresh.checkStatus && <p role="status">{refresh.checkStatus}</p>}
      {signedOutNote}
      {!(saving || refresh.busy) && (
        <MutationButton onClick={() => void refresh.recover()}>{refresh.action}</MutationButton>
      )}
    </section>
  );
}

/** Unsaved values belong to the old workspace. Keep them as text if Part identities change. */
export type PendingEdits = Map<string, string[]>;
export function usePendingEdits(pending: PendingEdits, key: string, values: string[]) {
  useLayoutEffect(() => {
    pending.set(key, values);
    return () => {
      pending.delete(key);
    };
  });
}
