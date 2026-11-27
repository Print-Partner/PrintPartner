import { routePath, routes, type BambuConnectStage } from "@print-partner/rust-client";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError } from "../core/rust";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

declare global {
  interface Window {
    __PRINT_PARTNER_NATIVE_BAMBU_CONNECT__?: (url: string) => Promise<void>;
  }
}

const EXTENSIONS = ".3mf,.gcode,.gco";
const MAX_BYTES = 64 * 1024 * 1024;

async function stageFile(build: number, file: File): Promise<BambuConnectStage> {
  const path = routePath("stageBambuConnect", { id: build });
  const body = new FormData();
  body.set("file", file);
  const response = await fetchCore(path, { method: "POST", body });
  const result = routes.stageBambuConnect.response.safeParse(
    await response.json().catch(() => undefined),
  );
  if (!result.success) throw new ContractError(path);
  return result.data;
}

function issue(error: unknown, fallback: string) {
  if (error instanceof CoreError && error.detail) return error.detail;
  return error instanceof Error ? error.message : typeof error === "string" ? error : fallback;
}

export function BambuConnectHandoff({ build }: { build: number }) {
  const openNative = window.__PRINT_PARTNER_NATIVE_BAMBU_CONNECT__;
  const enabled = useQuery({
    queryKey: ["core", "bambu-connect", build],
    enabled: openNative !== undefined,
    networkMode: "always",
    queryFn: async ({ signal }) => {
      const response = await fetchCore(`/api/v2/plans/${build}/features`, { signal });
      const features: { effective?: { bambu?: { enabled?: boolean } } } = await response.json();
      return features.effective?.bambu?.enabled === true;
    },
  });
  if (!openNative || enabled.data !== true) return null;
  return <HandoffForm build={build} openNative={openNative} />;
}

function HandoffForm({
  build,
  openNative,
}: {
  build: number;
  openNative: (url: string) => Promise<void>;
}) {
  const [file, setFile] = useState<File | null>(null);
  const offline = useConnection() === "offline";
  const stage = useMutation({
    mutationFn: (selected: File) => stageFile(build, selected),
    networkMode: "always",
    retry: false,
  });
  const open = useMutation({
    mutationFn: (url: string) => openNative(url),
    networkMode: "always",
    retry: false,
  });
  const busy = stage.isPending || open.isPending;
  const refusal = !file
    ? null
    : !/\.(3mf|gcode|gco)$/i.test(file.name) || file.size === 0
      ? "Choose a nonempty .3mf, .gcode.3mf, .gcode or .gco file."
      : file.size > MAX_BYTES
        ? "Print file exceeds the 64 MiB upload limit."
        : null;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (file && !refusal && !busy && !stage.data && !offline) stage.mutate(file);
  }

  return (
    <section aria-labelledby="bambu-connect-title">
      <h2 id="bambu-connect-title">Bambu Connect</h2>
      <p>Choose the printer and press Send in Bambu Connect. Nothing is sent from Print Partner.</p>
      <form onSubmit={submit}>
        <label>
          Sliced file for Bambu Connect{" "}
          <input
            type="file"
            accept={EXTENSIONS}
            disabled={busy}
            onChange={(event) => {
              stage.reset();
              open.reset();
              setFile(event.currentTarget.files?.[0] ?? null);
            }}
          />
        </label>{" "}
        {!stage.data && (
          <button
            type="submit"
            className="pp-button"
            disabled={!file || Boolean(refusal) || busy || offline}
          >
            {stage.isPending ? "Staging file…" : "Stage file"}
          </button>
        )}
      </form>
      {offline && !stage.data && <p>{OFFLINE_REASON}</p>}
      {refusal && <p role="alert">{refusal}</p>}
      {stage.isError && <p role="alert">{issue(stage.error, "Could not stage the file.")}</p>}
      {stage.data && (
        <>
          <p role="status">{stage.data.filename} is staged. Nothing has been sent.</p>
          <button
            type="button"
            className="pp-button"
            disabled={busy}
            onClick={() => {
              if (stage.data && !busy) open.mutate(stage.data.connect_url);
            }}
          >
            Open in Bambu Connect
          </button>
        </>
      )}
      {open.isError && <p role="alert">{issue(open.error, "Could not open Bambu Connect.")}</p>}
    </section>
  );
}
