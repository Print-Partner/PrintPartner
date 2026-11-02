import type {
  PrintOutcomeEvent,
  PrintRejectReason,
  PrintVerifyDecision,
  PrinterCheckoffLink,
} from "@print-partner/contracts";
import type { AppRepository } from "../db/repository.js";
import { pendingCheckoffUnits, unitKey } from "./printer-checkoff.js";
import { getPrinterCheckoffLink, updatePrinterCheckoffLink } from "./printer-checkoff-store.js";
import { isPrintRejectReason } from "./printer-outcomes-store.js";
import { acceptedPlanBasis, type AcceptedUnitDecision } from "../db/accepted-plan-progress.js";
import { parseRequiredUnitToken } from "./required-units.js";
import { readAdditionalDecisions } from "./imported-print-inventory.js";

type VerifyPrinterCheckoffResult = {
  link: PrinterCheckoffLink;
  units_confirmed: number;
  units_rejected: number;
  outcomes: PrintOutcomeEvent[];
};

function parseDecisions(
  raw: unknown,
  allowEmpty = false,
): PrintVerifyDecision[] | { error: string } {
  if (!Array.isArray(raw) || (!allowEmpty && raw.length === 0)) {
    return { error: "decisions required (reject needs a reason)" };
  }
  const out: PrintVerifyDecision[] = [];
  const seen = new Set<string>();
  for (let i = 0; i < raw.length; i += 1) {
    const item = raw[i];
    if (!item || typeof item !== "object") {
      return { error: `decisions[${i}] is invalid` };
    }
    const row = item as Record<string, unknown>;
    const partId = Number(row.part_id);
    const unitIndex = Number(row.unit_index);
    const result = row.result;
    if (!Number.isInteger(partId) || partId <= 0) {
      return { error: `decisions[${i}].part_id is invalid` };
    }
    if (!Number.isInteger(unitIndex) || unitIndex < 0) {
      return { error: `decisions[${i}].unit_index is invalid` };
    }
    if (result !== "confirmed" && result !== "rejected") {
      return { error: `decisions[${i}].result must be confirmed or rejected` };
    }
    const key = unitKey(partId, unitIndex);
    if (seen.has(key)) {
      return { error: `decisions[${i}] duplicates another unit` };
    }
    seen.add(key);
    const decision: PrintVerifyDecision = {
      part_id: partId,
      unit_index: unitIndex,
      result,
    };
    if (result === "rejected") {
      if (!isPrintRejectReason(row.reason)) {
        return { error: `decisions[${i}] reject needs a valid reason` };
      }
      decision.reason = row.reason as PrintRejectReason;
    } else if (row.reason != null && row.reason !== "") {
      if (!isPrintRejectReason(row.reason)) {
        return { error: `decisions[${i}].reason is invalid` };
      }
      decision.reason = row.reason;
    }
    if (row.note != null && row.note !== "") {
      if (typeof row.note !== "string") {
        return { error: `decisions[${i}].note is invalid` };
      }
      decision.note = row.note.trim().slice(0, 500) || undefined;
    }
    out.push(decision);
  }
  return out;
}

export function verifyPrinterCheckoff(
  repo: AppRepository,
  linkId: string,
  rawDecisions: unknown,
  rawAdditionalDecisions?: unknown,
): VerifyPrinterCheckoffResult | { error: string; status: number } {
  if (!repo.canMutateAcceptedPlan()) {
    return { error: "Accepted Plan update is unavailable", status: 503 };
  }
  const link = getPrinterCheckoffLink(repo, linkId);
  if (!link) return { error: "Checkoff link not found", status: 404 };
  if (link.state !== "awaiting_verify") {
    return { error: "Link is not awaiting verify", status: 409 };
  }

  const additionalDecisions = readAdditionalDecisions(rawAdditionalDecisions);
  if (!additionalDecisions) return { error: "Invalid additional part decisions", status: 400 };
  const parsed = parseDecisions(rawDecisions, additionalDecisions.length > 0);
  if ("error" in parsed) {
    return { error: parsed.error, status: 400 };
  }
  const decisions = parsed;

  const pending = new Set(pendingCheckoffUnits(link).map((u) => unitKey(u.part_id, u.unit_index)));
  for (const d of decisions) {
    if (!pending.has(unitKey(d.part_id, d.unit_index))) {
      return { error: "All decisions must target pending units on this link", status: 400 };
    }
  }

  const accepted = repo.readAcceptedPlanOperationalSnapshot(link.profile_id);
  if (accepted.kind !== "ready") {
    if (accepted.kind === "empty") {
      return { error: "Accepted Plan has no required units", status: 409 };
    }
    return {
      error:
        accepted.kind === "compatibility_dirty"
          ? "Accepted Plan requires compatibility repair"
          : "Accepted Plan operational state is not initialized",
      status: 409,
    };
  }
  const byCoordinate = new Map<string, (typeof accepted.snapshot.parts)[number]["units"][number]>(
    accepted.snapshot.parts.flatMap((part) =>
      part.units.map((unit) => [`${part.projectionPartId}:${unit.unitIndex}`, unit] as const),
    ),
  );
  if (link.units.some((unit) => !byCoordinate.has(unitKey(unit.part_id, unit.unit_index)))) {
    return { error: "Legacy Checkoff link no longer maps to the accepted Plan", status: 409 };
  }
  const tokenDecisions: AcceptedUnitDecision[] = decisions.map((decision) => {
    const unit = byCoordinate.get(unitKey(decision.part_id, decision.unit_index));
    if (!unit) throw new Error("Accepted Checkoff decision mapping failed");
    const token = parseRequiredUnitToken(unit.token);
    return decision.result === "rejected"
      ? {
          token,
          result: "rejected",
          reason: decision.reason!,
          ...(decision.note ? { note: decision.note } : {}),
        }
      : {
          token,
          result: "confirmed",
          ...(decision.note ? { note: decision.note } : {}),
        };
  });
  const result = repo.verifyAcceptedPrint({
    expected: acceptedPlanBasis(accepted.snapshot),
    linkId: link.id,
    expectedLink: link,
    decisions: tokenDecisions,
    additionalDecisions,
  });
  if (result.kind === "verified") {
    return {
      link: result.link,
      units_confirmed: result.unitsConfirmed,
      units_rejected: result.unitsRejected,
      outcomes: [...result.outcomes],
    };
  }
  if (result.kind === "link_not_found") return { error: "Checkoff link not found", status: 404 };
  if (result.kind === "link_changed") return { error: "Link changed concurrently", status: 409 };
  if (result.kind === "not_awaiting_verify") {
    return { error: "Link is not awaiting verify", status: 409 };
  }
  if (result.kind === "accepted_state_unavailable") {
    return {
      error:
        result.reason === "compatibility_dirty"
          ? "Accepted Plan requires compatibility repair"
          : "Accepted Plan operational state is not initialized",
      status: 409,
    };
  }
  if (result.kind === "stale_accepted_plan") {
    return { error: "Accepted Plan changed; reload and retry", status: 409 };
  }
  if (result.kind === "transaction_unavailable") {
    return { error: "Accepted Plan update is unavailable", status: 503 };
  }
  if (result.kind === "plan_archived") {
    return { error: "Archived Plan Progress cannot be changed", status: 409 };
  }
  return {
    error: "Confirm must include lower incomplete units first (Progress fills from the left)",
    status: 400,
  };
}

export function dismissHostFailedLink(
  repo: AppRepository,
  linkId: string,
): PrinterCheckoffLink | { error: string; status: number } {
  const link = getPrinterCheckoffLink(repo, linkId);
  if (!link) return { error: "Checkoff link not found", status: 404 };
  if (link.state !== "host_failed") {
    return { error: "Link is not host_failed", status: 409 };
  }
  const updated = updatePrinterCheckoffLink(
    repo,
    linkId,
    { state: "dismissed" },
    { requireState: "host_failed" },
  );
  if (!updated) return { error: "Link changed concurrently", status: 409 };
  return updated;
}
