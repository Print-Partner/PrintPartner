import { z } from "zod";
import type { PrinterCheckoffLink } from "./index.js";

const decisionSchema = z.strictObject({
  part_id: z.number().int().positive(),
  unit_index: z.number().int().nonnegative(),
  result: z.enum(["confirmed", "rejected"]),
  reason: z.string().optional(),
  note: z.string().optional(),
});

const bedSchema = z.object({
  link_id: z.string().trim().min(1),
  decisions: z.array(decisionSchema),
  additional_decisions: z.array(z.object({ index: z.number().int().nonnegative() }).loose()).optional(),
});

/**
 * `POST /printer-checkoff/verify-batch`: one Checkoff burst over several beds.
 * Repeating it with the same `idempotency_key` returns the first receipt.
 */
export const verifyBatchRequestSchema = z
  .strictObject({
    idempotency_key: z.string().trim().min(1).max(200),
    beds: z.array(bedSchema).min(1).max(50),
  })
  .refine((request) => new Set(request.beds.map((bed) => bed.link_id.trim())).size === request.beds.length, {
    message: "Each bed names a different link",
  });

export type VerifyBatchRequest = z.infer<typeof verifyBatchRequestSchema>;
export type VerifyBatchBed = VerifyBatchRequest["beds"][number];

export function parseVerifyBatchRequest(value: unknown): VerifyBatchRequest {
  return verifyBatchRequestSchema.parse(value);
}

type CheckoffLinkUnits = Pick<PrinterCheckoffLink, "id" | "state" | "units" | "resolved_units">;

function pendingUnits(link: CheckoffLinkUnits) {
  if (link.state !== "awaiting_verify") return [];
  const resolved = new Set((link.resolved_units ?? []).map((unit) => `${unit.part_id}:${unit.unit_index}`));
  return link.units.filter((unit) => !resolved.has(`${unit.part_id}:${unit.unit_index}`));
}

function bed(link: CheckoffLinkUnits, units: ReturnType<typeof pendingUnits>): VerifyBatchBed {
  return {
    link_id: link.id,
    decisions: units.map((unit) => ({ part_id: unit.part_id, unit_index: unit.unit_index, result: "confirmed" })),
  };
}

/** `P`: confirm every pending unit on one bed. */
export function plateBed(link: CheckoffLinkUnits): VerifyBatchBed {
  return bed(link, pendingUnits(link));
}

/** `F`: confirm one Part's pending units on every bed that holds them. */
export function partFillBeds(links: readonly CheckoffLinkUnits[], partId: number): VerifyBatchBed[] {
  return links.flatMap((link) => {
    const units = pendingUnits(link).filter((unit) => unit.part_id === partId);
    return units.length ? [bed(link, units)] : [];
  });
}

/** One request from any mix of beds; decisions and imported-extra decisions for the same bed merge. */
export function verifyBatchRequest(idempotencyKey: string, beds: readonly VerifyBatchBed[]): VerifyBatchRequest {
  const merged = new Map<string, VerifyBatchBed>();
  for (const next of beds) {
    const current = merged.get(next.link_id) ?? { link_id: next.link_id, decisions: [] };
    const seen = new Set(current.decisions.map((d) => `${d.part_id}:${d.unit_index}`));
    current.decisions.push(...next.decisions.filter((d) => !seen.has(`${d.part_id}:${d.unit_index}`)));
    if (next.additional_decisions?.length) {
      const extras = current.additional_decisions ?? [];
      const indexes = new Set(extras.map((d) => d.index));
      extras.push(...next.additional_decisions.filter((d) => !indexes.has(d.index)));
      current.additional_decisions = extras;
    }
    merged.set(next.link_id, current);
  }
  return parseVerifyBatchRequest({ idempotency_key: idempotencyKey, beds: [...merged.values()] });
}
