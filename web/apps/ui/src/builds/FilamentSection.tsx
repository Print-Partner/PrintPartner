import type { DeductionView, FilamentShortfalls } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useId, type ReactNode } from "react";
import { deductionsQuery } from "../core/deductions";
import { readCore } from "../core/rust";
import { useConnection } from "../frame/connection";
import { LoadedAt, ModuleOff, MutationButton } from "../screen/ScreenTemplate";
import {
  DeductionFact,
  DeductionNotices,
  newestOnSpools,
} from "./FilamentDeductions";

type Filament = Extract<FilamentShortfalls, { kind: "ready" }>["filaments"][number];
type Needed = Filament["grams_needed"];
type Available = Filament["grams_available"];

/**
 * `GET /api/v2/plans/{id}/filament-shortfalls`. Every read asks Spoolman, so it runs when
 * the Overview opens and when the owner presses Refresh, never on a timer or on focus.
 */
const filamentShortfallsQuery = (id: number) => ({
  queryKey: ["core", "filament-shortfalls", id],
  queryFn: ({ signal }: { signal: AbortSignal }) =>
    readCore("readFilamentShortfalls", { id }, signal),
  refetchOnReconnect: false,
  // A retry would ask Spoolman again; the owner presses Retry instead.
  retry: false,
});

const NEED_UNKNOWN: Record<Extract<Needed, { kind: "unknown" }>["reason"], string> = {
  grams_not_recorded: "no print of this Build has recorded its grams",
  part_not_printed: "a Part has no print with recorded grams",
};

const STOCK_UNKNOWN: Record<Extract<Available, { kind: "unknown" }>["reason"], string> = {
  not_spoolman: "this filament is not from Spoolman",
  integration_missing: "the Spoolman integration it names is not set up",
  integration_disabled: "its Spoolman integration is turned off",
  unreachable: "Spoolman could not be reached",
  filament_missing: "Spoolman has no filament with this ID",
  spool_weight_missing: "a spool of it reports no remaining weight",
};

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** The grams still needed and where the number comes from, or why it is not known. */
export function neededText(needed: Needed): { value: string; basis: string } {
  switch (needed.kind) {
    case "measured":
      return {
        value: `${needed.grams} g`,
        basis: `measured over ${count(needed.job_count, "print", "prints")}`,
      };
    case "estimated":
      return {
        value: `${needed.grams} g`,
        basis: needed.reason
          ? "slicer estimate; a printer's measured length could not be turned into grams"
          : "slicer estimate",
      };
    case "unknown":
      return { value: "Not known yet", basis: NEED_UNKNOWN[needed.reason] };
  }
}

/** Spoolman's grams and spool count, or why stock is not known. Never "0 g" for unknown. */
export function stockText(available: Available): { value: string; basis: string } {
  if (available.kind === "unknown") {
    return { value: "Not known", basis: STOCK_UNKNOWN[available.reason] };
  }
  const spools = available.spool_ids.length;
  return {
    value: `${available.grams} g`,
    basis: spools === 0 ? "no spools" : `on ${count(spools, "spool", "spools")}`,
  };
}

/** The filament's name: Spoolman's or the catalog label, else its filament ID. */
function filamentName(filament: Filament): string {
  const label = filament.label.trim();
  if (label) return label;
  return filament.filament_color_id || "No filament chosen";
}

function Status({ filament }: { filament: Filament }) {
  // An unknown side never shows "Short", whatever the status says.
  const known =
    filament.grams_needed.kind !== "unknown" && filament.grams_available.kind !== "unknown";
  if (!known || filament.status.kind === "unknown") return <>Not known</>;
  if (filament.status.kind === "short") {
    return <span className="pp-tag pp-tag-flush">Short by {filament.status.short_grams} g</span>;
  }
  return <>Enough</>;
}

function Amount({ term, text }: { term: string; text: { value: string; basis: string } }) {
  return (
    <div className="pp-fact">
      <dt>{term}</dt>
      <dd>
        {text.value}
        <span className="pp-fact-detail">{text.basis}</span>
      </dd>
    </div>
  );
}

function FilamentRow({
  filament,
  deductions,
}: {
  filament: Filament;
  deductions: DeductionView[];
}) {
  const nameId = useId();
  const name = filamentName(filament);
  const available = filament.grams_available;
  const deduction =
    available.kind === "spoolman" ? newestOnSpools(deductions, available.spool_ids) : undefined;
  return (
    <li aria-labelledby={nameId}>
      <h3 id={nameId}>{name}</h3>
      <dl className="pp-facts pp-filament-facts">
        <Amount term="Needed" text={neededText(filament.grams_needed)} />
        <Amount term="In Spoolman" text={stockText(filament.grams_available)} />
        <div className="pp-fact">
          <dt>Status</dt>
          <dd>
            <Status filament={filament} />
          </dd>
        </div>
        {deduction && <DeductionFact deduction={deduction} />}
      </dl>
      <ul className="pp-filament-parts" aria-label={`Parts on ${name}`}>
        {filament.parts.map((part) => (
          <li key={part.part_id}>
            {part.filename}{" "}
            <span className="pp-muted">
              {count(part.remaining_units, "unit left", "units left")}
            </span>
          </li>
        ))}
      </ul>
    </li>
  );
}

function Body({ data, deductions }: { data: FilamentShortfalls; deductions: DeductionView[] }) {
  switch (data.kind) {
    case "module_off":
      return (
        <ModuleOff
          module="Spoolman"
          wouldShow="the filament this Build still needs beside what Spoolman holds"
          where="the Overview"
        />
      );
    case "no_accepted_plan":
      return <p>No accepted Plan yet.</p>;
    case "ready":
      return (
        <>
          <DeductionNotices rows={deductions} />
          {data.filaments.length === 0 ? (
            <p>Nothing left to print.</p>
          ) : (
            <ul className="pp-filaments" aria-label="Filaments">
              {data.filaments.map((filament) => (
                <FilamentRow
                  key={filament.filament_color_id}
                  filament={filament}
                  deductions={deductions}
                />
              ))}
            </ul>
          )}
        </>
      );
  }
}

/**
 * The Overview's Filament section (GRE-1102): Spoolman shortfalls for the accepted Plan and
 * this Build's deductions. It loads on its own, so the rest of the Overview never waits for
 * Spoolman. The deduction list is read only after the shortfall read answered `ready`, so
 * with the Module off the section never asks for it.
 */
export function FilamentSection({ id }: { id: number }) {
  const headingId = useId();
  const offline = useConnection() === "offline";
  const query = useQuery(filamentShortfallsQuery(id));
  const data = query.data;
  const ready = data?.kind === "ready";
  const deductions = useQuery({
    ...deductionsQuery,
    enabled: ready,
    select: (list) => list.deductions.filter((row) => row.profile_id === id),
  });
  const refresh = async () => {
    // A read that turns `ready` enables the deduction query, and that reads by itself.
    const wasReady = ready;
    const read = await query.refetch({ cancelRefetch: false });
    if (wasReady && read.data?.kind === "ready") void deductions.refetch({ cancelRefetch: false });
  };
  let status: ReactNode = null;
  if (query.isFetching) {
    status = <span className="pp-muted">Reading Spoolman</span>;
  } else if (offline && !data) {
    status = "Not loaded yet.";
  } else if (query.isError && !offline) {
    status = (
      <>
        {data ? "Could not refresh" : "Could not load"}: {query.error.message}.{" "}
        {data && <LoadedAt prefix="Showing numbers read" at={query.dataUpdatedAt} />}
      </>
    );
  } else if (data?.kind === "ready") {
    status = <LoadedAt prefix="Read" at={query.dataUpdatedAt} />;
  }
  return (
    <section aria-labelledby={headingId} className="pp-filament-section">
      <h2 id={headingId}>Filament</h2>
      {data?.kind !== "module_off" && (
        <p className="pp-muted">
          Checking off a linked print deducts its filament from the Part's pinned spool in Spoolman.
        </p>
      )}
      <p className="pp-filament-status">
        <span role="status">{status}</span>{" "}
        <MutationButton onClick={() => void refresh()}>
          {query.isError && !offline ? "Retry" : "Refresh"}
        </MutationButton>
      </p>
      {ready && deductions.isError && (
        <p className="pp-muted">Could not read deductions: {deductions.error.message}.</p>
      )}
      {data && <Body data={data} deductions={(ready && deductions.data) || []} />}
    </section>
  );
}
