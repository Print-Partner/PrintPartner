/**
 * Prefer stored plan_id; unbound binds once to the active spine; never steal a bound job.
 * (`profile_id` in checkoff links is the plan id.)
 */
export function resolvePlanIdForPrinterFetch(
  storedPlanId: number | null | undefined,
  activeSpinePlanId: number | null | undefined,
): number | null {
  if (typeof storedPlanId === "number" && Number.isInteger(storedPlanId) && storedPlanId > 0) {
    return storedPlanId;
  }
  if (
    typeof activeSpinePlanId === "number" &&
    Number.isInteger(activeSpinePlanId) &&
    activeSpinePlanId > 0
  ) {
    return activeSpinePlanId;
  }
  return null;
}
