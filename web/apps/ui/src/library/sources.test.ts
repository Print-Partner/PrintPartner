import type { ProfileSummary } from "@print-partner/rust-client";
import { describe, expect, it } from "vitest";
import { acceptedRevisionText, type Source } from "./sources";

type Freshness = ProfileSummary["freshness"];
const source = { id: 2, current_source_revision_id: 42 } as Source;
const accepted = { accepted_at: "2026-11-01T10:00:00.000Z", accepted_input_set_id: 3 };
const named = { source_id: 2, source_name: "Hinge pack" };
const other = { source_id: 9, source_name: "Other" };
const stale = (
  reasons: Extract<Freshness, { status: "stale" }>["reasons"],
  untracked_sources: Extract<Freshness, { status: "stale" }>["untracked_sources"] = [],
): Freshness => ({ status: "stale", ...accepted, reasons, untracked_sources });
const text = (freshness: Freshness) => acceptedRevisionText(freshness, source);

describe("acceptedRevisionText", () => {
  it("reads a current Plan as the current revision", () => {
    expect(text({ status: "current", ...accepted })).toEqual({
      label: "The current revision",
      detail: "Revision 42",
    });
  });

  it("reads an untracked Plan per Source", () => {
    const untracked = (source_id: number): Freshness => ({
      status: "untracked",
      ...accepted,
      reasons: [{ kind: "source_revision_untracked", source_id, source_name: "x" }],
    });
    expect(text(untracked(2)).label).toBe("Not tracked by revision");
    // Another Source is untracked; this one was accepted at its current revision.
    expect(text(untracked(9)).label).toBe("The current revision");
    expect(
      text({
        status: "untracked",
        accepted_at: null,
        accepted_input_set_id: null,
        reasons: [{ kind: "no_accepted_inputs" }],
      }).label,
    ).toBe("No Plan accepted yet");
  });

  it("names the accepted revision when it changed or is gone, before naming changes", () => {
    expect(
      text(
        stale([
          { kind: "naming_rules_changed", ...named, accepted_digest: "a", current_digest: "b" },
          {
            kind: "source_revision_changed",
            ...named,
            accepted_revision_id: 30,
            current_revision_id: 42,
          },
        ]),
      ),
    ).toEqual({ label: "Revision 30", detail: "The Source is now at revision 42" });
    expect(
      text(stale([{ kind: "source_revision_unavailable", ...named, accepted_revision_id: 7 }])),
    ).toEqual({ label: "Revision 7", detail: "That revision is no longer available" });
    expect(
      text(
        stale([
          { kind: "naming_rules_changed", ...named, accepted_digest: "a", current_digest: "b" },
        ]),
      ),
    ).toEqual({ label: "The current revision", detail: "Its naming rules changed since Accept" });
  });

  it("keeps the current revision when only another Source changed", () => {
    expect(
      text(
        stale([
          {
            kind: "source_revision_changed",
            ...other,
            accepted_revision_id: 1,
            current_revision_id: 2,
          },
        ]),
      ).label,
    ).toBe("The current revision");
  });

  it("says Not known for a Plan-wide change, and Not tracked for an untracked Source", () => {
    expect(text(stale([{ kind: "plan_configuration_changed" }]))).toEqual({
      label: "Not known",
      detail: "This Source may not be in the Accepted Plan",
    });
    expect(
      text(
        stale([{ kind: "plan_inputs_invalid" }], [{ kind: "source_revision_untracked", ...named }]),
      ).label,
    ).toBe("Not tracked by revision");
  });

  it("says when freshness is unavailable", () => {
    expect(text({ status: "unavailable", code: "integrity" }).label).toBe("Not known");
  });
});
