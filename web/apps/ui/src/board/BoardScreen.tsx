import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { Link } from "react-router";
import { readCore, writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { useConnection } from "../frame/connection";
import { ScreenTemplate } from "../screen/ScreenTemplate";
import { queryScreenState } from "../screen/queryState";

const boardKey = ["core", "board"] as const;
const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

function NoteForm({ close }: { close: () => void }) {
  const connection = useConnection();
  const builds = useQuery({
    queryKey: ["core", "board-builds"],
    queryFn: ({ signal }) => readCore("listBoardBuilds", {}, signal),
    enabled: connection !== "offline",
  });
  const client = useQueryClient();
  const [build, setBuild] = useState("");
  const [note, setNote] = useState("");
  const save = useMutation({
    mutationFn: () => writeCore("createBoardNote", {}, { build_id: Number(build), body: note }),
    retry: false,
    onSuccess: () => {
      close();
    },
    onSettled: () => client.invalidateQueries({ queryKey: boardKey }),
  });
  const trimmedNote = note.trim();
  const tooLong = new TextEncoder().encode(trimmedNote).length > 2_000;
  const enabled =
    connection === "online" &&
    builds.isSuccess &&
    builds.data.builds.some(({ id }) => String(id) === build) &&
    trimmedNote.length > 0 &&
    !tooLong &&
    !save.isPending;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (enabled) save.mutate();
  }

  return (
    <form className="pp-board-form" aria-label="New Build note" onSubmit={submit}>
      <label className="pp-field">
        Build
        <select
          value={build}
          onChange={(event) => setBuild(event.target.value)}
          disabled={!builds.isSuccess || save.isPending}
          required
        >
          <option value="">Choose a Build</option>
          {builds.data?.builds.map(({ id, name }) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {builds.isPending && connection !== "offline" && <p role="status">Loading Builds…</p>}
      {builds.isError && (
        <p role="alert">
          Could not load Builds.{" "}
          <button className="pp-button" type="button" onClick={() => void builds.refetch()}>
            Retry
          </button>
        </p>
      )}
      {builds.data?.builds.length === 0 && (
        <p>
          <Link to="/builds">Create a Build</Link> to add a note.
        </p>
      )}
      <label className="pp-field">
        Note
        <textarea
          value={note}
          onChange={(event) => setNote(event.target.value)}
          rows={4}
          disabled={save.isPending}
          aria-describedby="board-note-limit"
          aria-invalid={tooLong || undefined}
          required
        />
      </label>
      <p
        id="board-note-limit"
        role={tooLong ? "alert" : undefined}
        className={tooLong ? "pp-field-error" : undefined}
      >
        {tooLong
          ? "This note exceeds 2,000 bytes. Shorten it before saving."
          : "Up to 2,000 bytes. Some characters use more than one byte."}
      </p>
      {save.isError && (
        <p role="alert" className="pp-field-error">
          {reason(save.error)}
        </p>
      )}
      <div className="pp-dialog-actions">
        <button className="pp-button" type="button" onClick={close} disabled={save.isPending}>
          Cancel
        </button>
        <button className="pp-button pp-button-primary" type="submit" disabled={!enabled}>
          {save.isPending ? "Saving note…" : "Save note"}
        </button>
      </div>
    </form>
  );
}

export function BoardScreen() {
  const connection = useConnection();
  const [writing, setWriting] = useState(false);
  const feed = useQuery({
    queryKey: boardKey,
    queryFn: ({ signal }) => readCore("listBoardNotes", {}, signal),
    enabled: connection !== "offline",
  });
  return (
    <ScreenTemplate
      title="Board"
      subnav={
        <>
          <p>Private notes linked to your Builds.</p>
          {writing ? (
            <NoteForm close={() => setWriting(false)} />
          ) : (
            <button
              className="pp-button pp-button-primary"
              type="button"
              disabled={connection !== "online" || !feed.isSuccess}
              onClick={() => setWriting(true)}
            >
              Add note
            </button>
          )}
        </>
      }
      state={queryScreenState(feed, connection, ({ notes }) => notes.length === 0)}
      empty={{ message: "No Build notes yet." }}
      renderData={({ notes }) => (
        <ul className="pp-proposals">
          {notes.map((note) => (
            <li key={note.id} className="pp-proposal">
              <h2>
                <Link to={`/builds/${note.build_id}`}>{note.build_name}</Link>
              </h2>
              <p className="pp-board-note">{note.body}</p>
              <time dateTime={note.created_at}>{date.format(new Date(note.created_at))}</time>
            </li>
          ))}
        </ul>
      )}
    />
  );
}
