import type { ReactNode } from "react";
import { useParams } from "react-router";
import { ConnectionOverride, useConnection } from "../frame/connection";
import type { Destination } from "../frame/destinations";
import { MutationButton, ScreenTemplate, type ScreenState } from "./ScreenTemplate";

/** A destination before its slice lands: empty, or offline when the core is unreachable. */
export function DestinationScreen({
  destination,
  children,
}: {
  destination: Destination;
  children?: ReactNode;
}) {
  const offline = useConnection() === "offline";
  return (
    <ScreenTemplate
      title={destination.label}
      state={offline ? { kind: "offline" } : { kind: "empty" }}
      renderData={() => null}
      empty={{
        message: `This screen arrives in a later slice. It will hold ${destination.holds}.`,
        action: children,
      }}
    />
  );
}

type Sample = { name: string; printed: number; required: number }[];
const SAMPLE: Sample = [
  { name: "Sample Build A", printed: 12, required: 40 },
  { name: "Sample Build B", printed: 0, required: 18 },
];
// A fixed time, so screenshots of the specimen do not change between runs.
const UPDATED_AT = Date.UTC(2026, 10, 4, 18, 30);
const SAMPLE_STATES: Record<string, ScreenState<Sample>> = {
  loading: { kind: "loading" },
  empty: { kind: "empty" },
  error: {
    kind: "error",
    message: "The core answered 503.",
    lastData: SAMPLE,
    updatedAt: UPDATED_AT,
    retry: () => {},
  },
  "module-off": {
    kind: "module-off",
    module: "Farm",
    wouldShow: "printers, the queue and the farm log",
  },
  offline: { kind: "offline", lastData: SAMPLE, updatedAt: UPDATED_AT },
  ready: { kind: "ready", data: SAMPLE },
};

/** `/states/<kind>`, kind being a key of SAMPLE_STATES: one state on sample data, for screenshots. */
export function StatesSpecimen() {
  const { kind = "" } = useParams();
  const current = kind in SAMPLE_STATES ? kind : "loading";
  return (
    <ConnectionOverride status={current === "offline" ? "offline" : "online"}>
      <ScreenTemplate
        key={current}
        title="Sample Builds"
        state={SAMPLE_STATES[current]}
        renderData={(builds) => (
          <>
            <ul className="pp-sample-list">
              {builds.map((b) => (
                <li key={b.name}>
                  <span>{b.name}</span>
                  <span className="num">
                    {b.printed} of {b.required} printed
                  </span>
                </li>
              ))}
            </ul>
            <MutationButton primary onClick={() => {}}>
              Record a print
            </MutationButton>
          </>
        )}
        empty={{
          message: "No Builds yet.",
          action: <MutationButton onClick={() => {}}>New Build</MutationButton>,
        }}
      />
    </ConnectionOverride>
  );
}

export function NotFound() {
  return (
    <section className="pp-screen">
      <h1>Not found</h1>
      <p>This address has no screen.</p>
    </section>
  );
}
