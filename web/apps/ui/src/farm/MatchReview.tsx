import type { MatchSuggestionList, UnmatchedPrintList } from "@print-partner/rust-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { Link } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore, writeCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { MutationButton, ScreenTemplate, type ScreenState } from "../screen/ScreenTemplate";

type Suggestion = MatchSuggestionList["suggestions"][number];
type Confidence = Suggestion["confidence"];

/** One read of every unmatched print with its suggestion; it never confirms anything. */
const suggestionsQuery = {
  queryKey: ["core", "match-suggestions"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listMatchSuggestions", {}, signal),
} as const;

/** Printer names, so a row says "Voron 2.4" rather than its id. */
/** The unmatched prints, for each print's integration: a claim's scope in the core. */
const unmatchedQuery = {
  queryKey: ["core", "unmatched-prints"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listUnmatchedPrints", {}, signal),
} as const;

const printersQuery = {
  queryKey: ["core", "printers"],
  queryFn: ({ signal }: { signal: AbortSignal }) => readCore("listPrinters", {}, signal),
} as const;

const GROUPS: readonly { confidence: Confidence; title: string; does: string }[] = [
  { confidence: "exact", title: "Exact", does: "claim it for the suggested Build" },
  { confidence: "likely", title: "Likely", does: "claim it for the suggested Build" },
  { confidence: "none", title: "No suggestion", does: "dismiss it" },
];

// A claim whose print was claimed already, by an earlier Apply whose answer was lost or
// from another window. It is reported as already claimed, never as claimed now.
const ALREADY_CLAIMED = "Print already claimed";

const plural = (n: number, one: string) => `${n} ${n === 1 ? one : `${one}s`}`;

const BASIS_WORDS = {
  unit_tokens: "Required-unit names in the objects",
  object_list: "the object list",
  file_name: "the file name",
  time_window: "the printer's recent work",
} as const;

/** What one row's suggestion rests on, in words. */
const basisText = (s: Suggestion) =>
  s.confidence === "none" || s.evidence.basis.length === 0
    ? "Nothing points to one Build."
    : `From ${s.evidence.basis.map((b) => BASIS_WORDS[b]).join(", ")}.`;

/** A file name as the core compares it: trimmed basename, lower case. */
const baseName = (filename: string) => {
  const name = filename.trim().replaceAll("\\", "/").replace(/\/+$/, "");
  return name.slice(name.lastIndexOf("/") + 1).toLowerCase();
};

/**
 * Claims one print for its suggested Build: true when this request claimed it, false when
 * the core answers that it was already claimed. Any other failure throws.
 */
async function claimPrint(s: Suggestion) {
  try {
    await writeCore("claimUnattributedPrint", { id: s.print_id }, { profile_id: s.build!.id });
    return true;
  } catch (error) {
    if (error instanceof CoreError && error.status === 409 && error.detail === ALREADY_CLAIMED)
      return false;
    throw error;
  }
}

/** Whether Apply claims a checked row for its Build; otherwise it dismisses the row. */
const claims = (s: Suggestion) => s.confidence !== "none" && s.build !== null;

const listParts = (claim: number, dismiss: number, claimWord: string, dismissWord: string) =>
  [
    claim > 0 && `${claimWord} ${plural(claim, "print")}`,
    dismiss > 0 && `${dismissWord} ${plural(dismiss, "print")}`,
  ]
    .filter(Boolean)
    .join(" and ");

/**
 * The Match prints review: every finished print the app could not match, grouped by
 * confidence in words. Only Exact rows start checked (`starts_checked`), and nothing is
 * claimed or dismissed until a group's Apply is pressed. Each group has one Apply: it claims
 * each checked Exact or Likely print for its suggested Build, or dismisses each checked print
 * with no suggestion, one request per print, through the Rust claim and dismiss routes.
 */
export function MatchReview() {
  const connection = useConnection();
  const farm = useModule("farm");
  const client = useQueryClient();
  const suggestions = useQuery({ ...suggestionsQuery, enabled: farm?.state !== "off" });
  const printers = useQuery({ ...printersQuery, enabled: farm?.state !== "off" });
  const printerName = (id: string) => printers.data?.printers.find((p) => p.id === id)?.name ?? id;
  // Prints an Apply resolved, hidden at once so a stale row is never offered again.
  const [resolved, setResolved] = useState<ReadonlySet<string>>(new Set());
  const open = (d: MatchSuggestionList) => d.suggestions.filter((s) => !resolved.has(s.print_id));
  const state: ScreenState<MatchSuggestionList> =
    farm?.state === "off"
      ? { kind: "module-off", module: "Farm", wouldShow: "finished prints to match" }
      : queryScreenState(suggestions, connection, (d) => open(d).length === 0);
  const [checked, setChecked] = useState<ReadonlyMap<string, boolean>>(new Map());
  const [failures, setFailures] = useState<ReadonlyMap<string, string>>(new Map());
  // Set while Apply runs, so a second press waits instead of sending twice.
  const applying = useRef(false);
  const [result, setResult] = useState("");

  const isChecked = (s: Suggestion) => checked.get(s.print_id) ?? s.starts_checked;
  const setMany = (rows: readonly Suggestion[], on: boolean) =>
    setChecked((now) => new Map([...now, ...rows.map((s) => [s.print_id, on] as const)]));

  const apply = async (picked: readonly Suggestion[]) => {
    if (applying.current || picked.length === 0) return;
    applying.current = true;
    setResult("");
    const failed = new Map<string, string>();
    const done: string[] = [];
    let claimed = 0;
    let already = 0;
    let dismissed = 0;
    const rows = suggestions.data?.suggestions ?? [];
    // A claim claims every open print of the same file on the same integration, so reprints
    // of one file are claimed together. A failed read keeps the last list; the printer
    // stands in only when there is none.
    const prints = await client
      .fetchQuery({ ...unmatchedQuery, staleTime: 0 })
      .catch(() => client.getQueryData<UnmatchedPrintList>(unmatchedQuery.queryKey))
      .then((list) => list?.prints ?? []);
    const fileOf = (s: Suggestion) => {
      const integration = prints.find((p) => p.id === s.print_id)?.integration_id;
      return `${integration ?? `printer:${s.printer_id}`}:${baseName(s.filename)}`;
    };
    // A file this Apply claimed covers a later "already claimed" answer on the same integration,
    // or on the same file name when that print's integration could not be read.
    const claimKey = (s: Suggestion) =>
      prints.some((p) => p.id === s.print_id) ? fileOf(s) : baseName(s.filename);
    const claimedFiles = new Set<string>();
    for (const s of picked) {
      if (done.includes(s.print_id)) continue;
      try {
        if (claims(s)) {
          if (await claimPrint(s)) {
            const reprints = rows.filter(
              (r) =>
                !resolved.has(r.print_id) && !done.includes(r.print_id) && fileOf(r) === fileOf(s),
            );
            const ids = reprints.some((r) => r.print_id === s.print_id)
              ? reprints.map((r) => r.print_id)
              : [s.print_id];
            claimed += ids.length;
            claimedFiles.add(fileOf(s)).add(baseName(s.filename));
            done.push(...ids);
          } else {
            if (claimedFiles.has(claimKey(s))) claimed += 1;
            else already += 1;
            done.push(s.print_id);
          }
        } else {
          await writeCore("dismissUnattributedPrint", { id: s.print_id }, {});
          dismissed += 1;
          done.push(s.print_id);
        }
      } catch (error) {
        failed.set(
          s.print_id,
          error instanceof CoreError
            ? (error.detail ?? error.message)
            : "No answer from the core. Apply again to retry it.",
        );
      }
    }
    const tried = new Set(picked.map((s) => s.print_id));
    setFailures((now) => new Map([...[...now].filter(([id]) => !tried.has(id)), ...failed]));
    setChecked((now) => {
      const next = new Map([...now].filter(([id]) => !tried.has(id)));
      for (const id of failed.keys()) next.set(id, true);
      return next;
    });
    const applied = listParts(claimed, dismissed, "claimed", "dismissed");
    setResult(
      [
        applied && `Applied: ${applied}.`,
        already > 0 &&
          `${plural(already, "print")} ${already === 1 ? "was" : "were"} already claimed.`,
        failed.size > 0 && `${plural(failed.size, "print")} could not be applied.`,
      ]
        .filter(Boolean)
        .join(" "),
    );
    setResolved((now) => new Set([...now, ...done]));
    applying.current = false;
    await client.invalidateQueries({ queryKey: ["core"] });
  };

  // Outside the data rows, so the outcome of the Apply that empties the backlog still shows.
  const status = (
    <div role="status" aria-label="Match review result" className="pp-tally">
      {result && <p>{result}</p>}
    </div>
  );

  return (
    <ScreenTemplate
      title="Match prints"
      state={state}
      empty={{
        message: "Every finished print is matched.",
        action: (
          <>
            {status}
            <Link to="/farm" className="pp-link">
              Back to Farm
            </Link>
          </>
        ),
      }}
      renderData={(data) => {
        const rows = open(data);
        return (
          <>
            <p>
              Finished prints the app could not match to a Build. A checked Exact or Likely print is
              claimed for its suggested Build, and a checked print with no suggestion is dismissed.
              Only Exact rows start checked, and nothing changes until you press a group&apos;s
              Apply.
            </p>
            {status}
            {GROUPS.map(({ confidence, title, does }) => {
              const group = rows.filter((s) => s.confidence === confidence);
              if (group.length === 0) return null;
              const picked = group.filter(isChecked);
              const claimCount = picked.filter(claims).length;
              const headingId = `match-${confidence}`;
              return (
                <section key={confidence} aria-labelledby={headingId}>
                  <h2 id={headingId}>{`${title} (${group.length})`}</h2>
                  <p>{`Applying a checked row will ${does}.`}</p>
                  <p>
                    <button
                      type="button"
                      className="pp-button"
                      onClick={() => setMany(group, true)}
                    >
                      {`Check all ${title}`}
                    </button>{" "}
                    <button
                      type="button"
                      className="pp-button"
                      onClick={() => setMany(group, false)}
                    >
                      {`Uncheck all ${title}`}
                    </button>
                  </p>
                  <ul>
                    {group.map((s) => {
                      const target = claims(s)
                        ? `Claim for ${s.build!.name}${s.plate ? `, plate ${s.plate.ordinal}` : ""}`
                        : "Dismiss";
                      const error = failures.get(s.print_id);
                      return (
                        <li key={s.print_id}>
                          <label>
                            <input
                              type="checkbox"
                              checked={isChecked(s)}
                              onChange={(e) => setMany([s], e.currentTarget.checked)}
                            />{" "}
                            {`${s.filename} on ${printerName(s.printer_id)}`}
                          </label>
                          <p>
                            {`${target}. ${basisText(s)}`} Finished{" "}
                            <time dateTime={s.completed_at}>
                              {new Date(s.completed_at).toLocaleString()}
                            </time>
                            .
                          </p>
                          {error && <p>{error}</p>}
                        </li>
                      );
                    })}
                  </ul>
                  <p>
                    {picked.length === 0 ? (
                      `Nothing checked in ${title}.`
                    ) : (
                      <MutationButton primary onClick={() => void apply(picked)}>
                        {`Apply ${title}: ${listParts(claimCount, picked.length - claimCount, "claim", "dismiss")}`}
                      </MutationButton>
                    )}
                  </p>
                </section>
              );
            })}
          </>
        );
      }}
    />
  );
}
