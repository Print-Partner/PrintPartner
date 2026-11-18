import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { sourcesQuery } from "../core/sources";

/**
 * The core's create and attach refusals, in the UI's words. The core still says "Profile" and
 * "Project" for a Build and a Source; any other refusal is shown as the core gave it.
 */
export function refusalText(detail: string): string {
  if (detail.startsWith("Profile already exists")) {
    return "A Build with this name already exists. Choose another name.";
  }
  if (detail.startsWith("Profile name is")) return detail.replace("Profile", "Build");
  if (detail === "Project not found") {
    return "That Source is no longer in the Library. Choose another.";
  }
  if (detail.startsWith("Source ") && detail.endsWith("is already attached to this build")) {
    return "That Source is already on this Build. Choose another.";
  }
  return detail;
}

/**
 * The Library's Sources as a select. `null` is "No base Source" when `allowNone`, and
 * otherwise means nothing is chosen yet. While the Sources load, says so instead; when they
 * fail, says so and offers Try again.
 */
export function SourceSelect({
  id,
  value,
  onChange,
  allowNone,
}: {
  id: string;
  value: number | null;
  onChange: (sourceId: number | null) => void;
  allowNone: boolean;
}) {
  const sources = useQuery(sourcesQuery);
  if (!sources.data) {
    if (sources.isError && !sources.isFetching) {
      return (
        <>
          <p id={id} className="pp-muted">
            Could not load the Library's Sources: {sources.error.message}
          </p>
          <button type="button" className="pp-button" onClick={() => void sources.refetch()}>
            Try again
          </button>
        </>
      );
    }
    return (
      <p id={id} className="pp-muted">
        Loading the Library's Sources.
      </p>
    );
  }
  const list = sources.data.sources;
  if (list.length === 0) {
    return (
      <p id={id}>
        The Library has no Sources yet.{" "}
        <Link to="/library" className="pp-link">
          Add a Source in Library
        </Link>{" "}
        first{allowNone ? ", or create the Build without one" : ""}.
      </p>
    );
  }
  return (
    <select
      id={id}
      value={value ?? ""}
      onChange={(event) => onChange(event.target.value === "" ? null : Number(event.target.value))}
    >
      {(allowNone || value === null) && (
        <option value="">{allowNone ? "No base Source" : "Choose a Source"}</option>
      )}
      {list.map((source) => (
        <option key={source.id} value={source.id}>
          {source.name}
        </option>
      ))}
    </select>
  );
}
