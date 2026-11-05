import { useEffect, useId, useState, type ReactNode } from "react";
import { Link } from "react-router";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

/** The five states from docs/product/ui-ux-target.md (Screen states), plus "ready". */
export type ScreenState<T> =
  | { kind: "loading" }
  | { kind: "empty" }
  | { kind: "ready"; data: T }
  | { kind: "error"; message: string; lastData?: T; updatedAt?: number; retry: () => void }
  | { kind: "module-off"; module: string; wouldShow: string }
  | { kind: "offline"; lastData?: T; updatedAt?: number };

/** Skeletons appear only after this long, so fast reads never flash. */
export const SKELETON_DELAY_MS = 150;

// The rows exist from the first frame so the layout does not jump, and become visible
// after the delay. Nothing from the data renderer, so no false zeros.
function Loading({ title }: { title: string }) {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const timer = setTimeout(() => setVisible(true), SKELETON_DELAY_MS);
    return () => clearTimeout(timer);
  }, []);
  return (
    <div className="pp-loading" aria-busy="true" data-skeleton={visible ? "shown" : "reserved"}>
      <p role="status" className="pp-visually-hidden">
        Loading {title}
      </p>
      {[0, 1, 2].map((i) => (
        <div key={i} className="pp-skeleton-row" aria-hidden="true" />
      ))}
    </div>
  );
}

export function LoadedAt({ prefix, at }: { prefix: string; at?: number }) {
  if (at === undefined) return null;
  const date = new Date(at);
  const text = new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(date);
  return (
    <>
      {prefix} <time dateTime={date.toISOString()}>{text}</time>.
    </>
  );
}

/**
 * The button for any change. Offline it stays focusable but does nothing, and the reason
 * is shown beside it and set as its accessible description.
 */
export function MutationButton({
  children,
  onClick,
  primary = false,
}: {
  children: ReactNode;
  onClick: () => void;
  primary?: boolean;
}) {
  const offline = useConnection() === "offline";
  const reasonId = useId();
  return (
    <span className="pp-mutation">
      <button
        type="button"
        className={primary ? "pp-button pp-button-primary" : "pp-button"}
        aria-disabled={offline || undefined}
        aria-describedby={offline ? reasonId : undefined}
        onClick={offline ? undefined : onClick}
      >
        {children}
      </button>
      {offline && (
        <span id={reasonId} className="pp-mutation-reason">
          {OFFLINE_REASON}
        </span>
      )}
    </span>
  );
}

/**
 * Says which Module is off, what would appear, and links to Settings. The module-off
 * screen state uses it, and so does a screen that shows one Module's part inline.
 */
export function ModuleOff({
  module,
  wouldShow,
  where = "this screen",
}: {
  module: string;
  wouldShow: string;
  where?: string;
}) {
  return (
    <div className="pp-module-off">
      <p>The {module} module is off.</p>
      <p>
        When it is on, {where} shows {wouldShow}.
      </p>
      <Link to="/settings" className="pp-link">
        Turn it on in Settings
      </Link>
    </div>
  );
}

export function ScreenTemplate<T>({
  title,
  state,
  renderData,
  empty,
  subnav,
}: {
  title: string;
  /** Navigation that belongs under the heading in every state, such as a tab bar. */
  subnav?: ReactNode;
  state: ScreenState<T>;
  renderData: (data: T) => ReactNode;
  /** The empty state's message and its next useful action. */
  empty: { message: string; action?: ReactNode };
}) {
  const headingId = useId();
  let banner: ReactNode = null;
  let content: ReactNode;
  switch (state.kind) {
    case "loading":
      content = <Loading title={title} />;
      break;
    case "empty":
      content = (
        <>
          <p>{empty.message}</p>
          {empty.action}
        </>
      );
      break;
    case "ready":
      content = renderData(state.data);
      break;
    case "error":
      banner = (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>
            {state.lastData === undefined
              ? `Could not load ${title}.`
              : `Could not refresh ${title}.`}{" "}
            {state.lastData !== undefined && (
              <LoadedAt prefix="Showing data from" at={state.updatedAt} />
            )}
          </p>
          <p className="pp-banner-detail">{state.message}</p>
          <button type="button" className="pp-button" onClick={state.retry}>
            Retry
          </button>
        </div>
      );
      content = state.lastData !== undefined && renderData(state.lastData);
      break;
    case "module-off":
      content = <ModuleOff module={state.module} wouldShow={state.wouldShow} />;
      break;
    case "offline":
      banner = (
        <div role="status" className="pp-banner" data-kind="offline">
          <p>
            {OFFLINE_REASON}{" "}
            {state.lastData === undefined ? (
              `${title} has not loaded yet.`
            ) : (
              <LoadedAt prefix="Showing what was loaded at" at={state.updatedAt} />
            )}
          </p>
        </div>
      );
      content = state.lastData !== undefined && renderData(state.lastData);
      break;
  }
  return (
    <section aria-labelledby={headingId} className="pp-screen" data-state={state.kind}>
      <h1 id={headingId}>{title}</h1>
      {subnav}
      {banner}
      {content}
    </section>
  );
}
