import type { AcceptedPlanBasisContract, AcceptedPlatePrinter } from "@print-partner/contracts";
import type { PrinterMachine } from "@print-partner/domain";
import type { AcceptedPlanBasis } from "../db/accepted-plan-progress.js";

export function basisContract(basis: AcceptedPlanBasis): AcceptedPlanBasisContract {
  return {
    profile_id: basis.profileId,
    plan_version: basis.planVersion,
    plan_revision_id: basis.revisionId,
    plan_revision_digest: basis.revisionDigest,
    required_unit_mapping_digest: basis.requiredUnitMappingDigest,
  };
}

export function sameAcceptedPlanBasis(left: AcceptedPlanBasis, right: AcceptedPlanBasis): boolean {
  return (
    left.profileId === right.profileId &&
    left.planVersion === right.planVersion &&
    left.revisionId === right.revisionId &&
    left.revisionDigest === right.revisionDigest &&
    left.requiredUnitMappingDigest === right.requiredUnitMappingDigest
  );
}

export function millimetresToMicrometres(value: number): number | null {
  const converted = value * 1_000;
  return Number.isSafeInteger(converted) ? converted : null;
}

export function acceptedPrinter(machine: PrinterMachine): AcceptedPlatePrinter | null {
  const bedWidthUm = millimetresToMicrometres(machine.bed_width_mm);
  const bedDepthUm = millimetresToMicrometres(machine.bed_depth_mm);
  const bedHeightUm =
    machine.bed_height_mm == null ? null : millimetresToMicrometres(machine.bed_height_mm);
  const marginUm = millimetresToMicrometres(machine.margin_mm);
  const id = machine.id.trim();
  const name = machine.name.trim();
  const model = machine.model.trim();
  if (
    !id ||
    !name ||
    !model ||
    bedWidthUm == null ||
    bedWidthUm <= 0 ||
    bedDepthUm == null ||
    bedDepthUm <= 0 ||
    bedHeightUm == null ||
    bedHeightUm <= 0 ||
    marginUm == null ||
    marginUm < 0 ||
    marginUm * 2 >= bedWidthUm ||
    marginUm * 2 >= bedDepthUm
  ) {
    return null;
  }
  return {
    id,
    name,
    model,
    bed_width_um: bedWidthUm,
    bed_depth_um: bedDepthUm,
    bed_height_um: bedHeightUm,
    margin_um: marginUm,
  };
}

export function compareUtf8(left: string, right: string): number {
  return Buffer.compare(Buffer.from(left), Buffer.from(right));
}
