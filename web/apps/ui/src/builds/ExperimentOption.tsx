import type { ExperimentCopy } from "@print-partner/rust-client";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import { Link } from "react-router";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { resendable } from "../core/writes";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { refusalText } from "./plan";

/** A Build that holds the order a draft proposes. */
export interface OrderHolder {
  profileId: number;
  name: string;
}

/** The holding Builds a `duplicate_order` refusal names with their IDs, or none. */
export function holdersOf(error: Error | null): OrderHolder[] {
  if (!(error instanceof CoreError) || error.code !== "duplicate_order") return [];
  return error.conflictingBuilds.flatMap(({ name, profileId }) =>
    profileId === null ? [] : [{ profileId, name }],
  );
}

/** Rust's Build name limit (`BuildName::parse`), in Unicode code points after trimming. */
const NAME_MAX = 4096;
const SUFFIX = " experiment";
const codePoints = (text: string) => Array.from(text);

/** `<parent> experiment`, with the parent shortened so the whole name fits `NAME_MAX`. */
const defaultName = (parent: string) =>
  codePoints(parent)
    .slice(0, NAME_MAX - SUFFIX.length)
    .join("") + SUFFIX;

/** Why Start is off, or null when it can send. */
function blockedReason(offline: boolean, name: string, pending: boolean): string | null {
  if (offline) return OFFLINE_REASON;
  const trimmed = name.trim();
  if (!trimmed) return "An experiment needs a name.";
  if (codePoints(trimmed).length > NAME_MAX) {
    return `An experiment name can have at most ${NAME_MAX.toLocaleString("en-US")} characters.`;
  }
  return pending ? "Starting the experiment." : null;
}

// Duplication takes no idempotency key, so an unknown outcome is never resent: a second
// send could make a second experiment. The Builds list shows whether the first one landed.
const UNKNOWN =
  "The core could not confirm the experiment. Check the Builds list before you try again.";

/**
 * The experiment option beside a held order: a detached copy of a holding Build, made with
 * `POST /api/v2/plans/{id}/duplicate`. The copy takes no order number, and the holder's
 * Accepted Plan as a draft when it has one; `draft_id` is null when it has none. This draft
 * and the holding Build stay as they are.
 */
export function ExperimentOption({ holders }: { holders: readonly OrderHolder[] }) {
  const client = useQueryClient();
  const offline = useConnection() === "offline";
  const ids = { parent: useId(), name: useId(), reason: useId() };
  const [parentId, setParentId] = useState(holders[0]?.profileId);
  const parent = holders.find((h) => h.profileId === parentId) ?? holders[0];
  const [name, setName] = useState(() => (parent ? defaultName(parent.name) : ""));
  const create = useMutation({
    mutationFn: (body: { parent: OrderHolder; name: string }) =>
      writeCore("duplicateBuild", { id: body.parent.profileId }, { name: body.name }),
    retry: false,
    // Send now or fail now: a paused mutation would create the Build after the gate closed.
    networkMode: "always",
    onSuccess: () => client.invalidateQueries({ queryKey: ["core", "builds"] }),
  });
  if (!parent) return null;
  const blocked = blockedReason(offline, name, create.isPending);
  const copied: ExperimentCopy | undefined = create.data;
  return (
    <div className="pp-experiment">
      <p>
        To try another Plan for this order without taking it, start an experiment from the Build
        that holds it. The experiment has no order number. If that Build has an Accepted Plan, the
        experiment starts with a draft of it; otherwise it starts with no draft.
      </p>
      <div className="pp-fields">
        {holders.length > 1 && (
          <label className="pp-field" htmlFor={ids.parent}>
            <span>Start from</span>
            <select
              id={ids.parent}
              value={parent.profileId}
              onChange={(event) => {
                const next = holders.find((h) => h.profileId === Number(event.target.value));
                if (!next) return;
                if (name === defaultName(parent.name)) setName(defaultName(next.name));
                setParentId(next.profileId);
              }}
            >
              {holders.map((h) => (
                <option key={h.profileId} value={h.profileId}>
                  {h.name}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className="pp-field" htmlFor={ids.name}>
          <span>Experiment name</span>
          <input
            id={ids.name}
            type="text"
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
      </div>
      <div className="pp-dialog-actions">
        <button
          type="button"
          className="pp-button"
          aria-disabled={blocked ? true : undefined}
          aria-describedby={blocked ? ids.reason : undefined}
          onClick={() => !blocked && create.mutate({ parent, name: name.trim() })}
        >
          Start an experiment from {parent.name}
        </button>
        {blocked && (
          <span id={ids.reason} className="pp-mutation-reason">
            {blocked}
          </span>
        )}
      </div>
      {copied && (
        <p role="status">
          Started {create.variables?.name} as an experiment of {create.variables?.parent.name}.{" "}
          {copied.draft_id === null
            ? `${create.variables?.parent.name} has no Accepted Plan, so the experiment has no draft yet.`
            : `It has a draft of ${create.variables?.parent.name}'s Accepted Plan.`}{" "}
          This draft is unchanged.{" "}
          <Link to={`/builds/${copied.build_id}/plan`}>Open {create.variables?.name}</Link>
        </p>
      )}
      {create.isError && (
        <p role="alert" className="pp-field-error">
          {resendable(create.error) ? UNKNOWN : refusalText(create.error)}
        </p>
      )}
    </div>
  );
}
