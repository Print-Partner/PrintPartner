import { sql } from "drizzle-orm";
import { StatusExecution } from "../integrations/status-execution.js";
/**
 * Golden MCP proposal lifecycle transcripts (GRE-577).
 *
 * Drives the real product MCP server (real SQLite repository, real job
 * runner, real apply path) on a fresh temp data dir per scenario and records
 * each request with its normalized response. Ids, timestamps, and the temp
 * path are replaced with stable placeholders, then compared byte-for-byte to
 * fixtures/parity/mcp-proposals/<scenario>.json. Any drift fails the test.
 *
 * Regenerate after an intentional contract change:
 *   PP_UPDATE_PARITY_FIXTURES=1 npx vitest run src/mcp/proposal-transcripts.test.ts
 */

import { afterEach, describe, expect, it } from "vitest";
import { copyFileSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import type { AssistantProposedAction } from "@print-partner/contracts";
import { loadConfig } from "../config.js";
import { getDb, SqliteDatabase } from "../db/client.js";
import { AppRepository } from "../db/repository.js";
import { createJobRunner } from "../services/job-runner.js";
import { listCustomFilaments } from "../services/custom-filaments.js";
import { createProductMcpServer } from "./product-mcp.js";

const FIXTURE_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "../../../../../fixtures/parity/mcp-proposals",
);
const UPDATE = process.env.PP_UPDATE_PARITY_FIXTURES === "1";

const UUID_RE = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi;
const CUSTOM_FILAMENT_ID_RE = /\bcustom-[0-9a-f]{8}\b/g;
const ISO_TIMESTAMP_RE = /\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})\b/g;
const UNKNOWN_ACTION_ID = "00000000-0000-4000-8000-000000000000";

type Step = {
  request: { method: "tools/call"; params: { name: string; arguments: Record<string, unknown> } };
  response: { isError: boolean; content: { type: string; json?: unknown; text?: string }[] };
};

type Session = {
  call: (name: string, args?: Record<string, unknown>) => Promise<Record<string, unknown>>;
  /** Raw (un-normalized) action id from the most recent propose result. */
  lastProposedId: () => string;
  pending: Map<string, AssistantProposedAction>;
  dataDir: string;
  steps: Step[];
};

/** Maps volatile values to placeholders numbered by first appearance. */
class Normalizer {
  private readonly seen = new Map<string, string>();
  private readonly counters = new Map<string, number>();

  constructor(private readonly dataDir: string) {}

  private token(kind: string, raw: string): string {
    const known = this.seen.get(raw);
    if (known) return known;
    const n = (this.counters.get(kind) ?? 0) + 1;
    this.counters.set(kind, n);
    const placeholder = `<${kind}-${n}>`;
    this.seen.set(raw, placeholder);
    return placeholder;
  }

  string(value: string): string {
    return value
      .split(this.dataDir)
      .join("<data-dir>")
      .replace(UUID_RE, (m) => (m === UNKNOWN_ACTION_ID ? m : this.token("action-id", m)))
      .replace(CUSTOM_FILAMENT_ID_RE, (m) => this.token("custom-filament-id", m))
      .replace(ISO_TIMESTAMP_RE, "<timestamp>");
  }

  value(value: unknown): unknown {
    if (typeof value === "string") return this.string(value);
    if (Array.isArray(value)) return value.map((v) => this.value(v));
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value).map(([k, v]) => [this.string(k), this.value(v)]),
      );
    }
    return value;
  }
}

async function runScenario(
  scenario: string,
  description: string,
  body: (session: Session) => Promise<void>,
  seed?: string,
): Promise<{ transcript: Record<string, unknown>; dataDir: string; cleanup: () => Promise<void> }> {
  const dataDir = mkdtempSync(join(tmpdir(), "pp-mcp-transcript-"));
  const sqlite = new SqliteDatabase(dataDir);
  if (seed) copyFileSync(resolve(FIXTURE_DIR,"../../../rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db"),sqlite.dbPath);
  sqlite.connect();
  if (seed) {
    for (const statement of seed.replaceAll("<data-dir>", dataDir.replaceAll("'", "''")).split(";").filter((s) => s.trim())) getDb(sqlite).run(sql.raw(statement));
    mkdirSync(join(dataDir,"repos/2"), {recursive:true});
    const documents = JSON.parse(readFileSync(join(FIXTURE_DIR,"build-planning/documents.json"),"utf8")) as {sources: {source_name:string}[]};
    for (const source of documents.sources) {
      const root = join(dataDir,"assistant-domain/sources",source.source_name);
      mkdirSync(root,{recursive:true});writeFileSync(join(root,"compatibility.yaml"),JSON.stringify(source));
    }
  }
  const repo = new AppRepository(getDb(sqlite), undefined, sqlite.reposDir);
  const getRepo = () => repo;
  const config = { ...loadConfig(), dataDir };
  const pending = new Map<string, AssistantProposedAction>();
  const server = createProductMcpServer({
    statusExecution: new StatusExecution(),
    getRepo,
    jobs: createJobRunner(getRepo, dataDir),
    config,
    pending,
  });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "gre-577-transcripts", version: "0.0.1" });
  await server.connect(serverTransport);
  await client.connect(clientTransport);

  const normalizer = new Normalizer(dataDir);
  const steps: Step[] = [];
  let lastProposedId = "";
  const session: Session = {
    pending,
    dataDir,
    steps,
    lastProposedId: () => lastProposedId,
    async call(name, args = {}) {
      const result = await client.callTool({ name, arguments: args });
      const content = (result.content as { type: string; text?: string }[]).map((c) => {
        if (c.type !== "text" || c.text === undefined) return { type: c.type };
        if (seed) return {type:c.type,text:c.text};
        try {
          return { type: c.type, json: JSON.parse(c.text) as unknown };
        } catch {
          return { type: c.type, text: c.text };
        }
      });
      const first = (seed ? JSON.parse((result.content as {text:string}[])[0]!.text) : content[0]?.json) as { action?: { id?: string } } | undefined;
      if (typeof first?.action?.id === "string") lastProposedId = first.action.id;
      steps.push({
        request: { method: "tools/call", params: { name, arguments: args } },
        response: { isError: result.isError === true, content },
      });
      return (first ?? {}) as Record<string, unknown>;
    },
  };

  await body(session);

  const transcript = normalizer.value({
    scenario,
    description,
    steps,
    final_state: {
      pending_action_ids: [...pending.keys()],
      custom_filaments: listCustomFilaments(dataDir),
    },
  }) as Record<string, unknown>;

  return {
    transcript,
    dataDir,
    cleanup: async () => {
      await client.close();
      await server.close();
      sqlite.close();
      rmSync(dataDir, { recursive: true, force: true });
    },
  };
}

function checkGolden(scenario: string, transcript: Record<string, unknown>): void {
  const path = join(FIXTURE_DIR, `${scenario}.json`);
  const actual = `${JSON.stringify(transcript, null, 2)}\n`;
  if (UPDATE) {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, actual, "utf8");
    return;
  }
  let expected: string;
  try {
    expected = readFileSync(path, "utf8");
  } catch {
    throw new Error(
      `Missing golden transcript ${path}; regenerate with PP_UPDATE_PARITY_FIXTURES=1`,
    );
  }
  expect(
    actual,
    `MCP transcript drift in ${scenario}.json (PP_UPDATE_PARITY_FIXTURES=1 regenerates)`,
  ).toBe(expected);
}

const FILAMENT = { display_name: "Galaxy Black", hex: "#1a1a1a", product_line: "PLA Basic" };

const SCENARIOS: {
  name: string;
  description: string;
  body: (s: Session) => Promise<void>;
  assert: (s: Session) => void;
}[] = [
  {
    name: "propose-list-confirm",
    description:
      "propose -> list_pending_actions -> confirm_apply applies exactly once and clears the pending entry",
    body: async (s) => {
      await s.call("propose_add_custom_filament", FILAMENT);
      const id = s.lastProposedId();
      await s.call("list_pending_actions");
      await s.call("confirm_apply", { action_id: id });
      await s.call("list_pending_actions");
    },
    assert: (s) => {
      expect(s.pending.size).toBe(0);
      expect(listCustomFilaments(s.dataDir)).toHaveLength(1);
      expect(s.steps.map((step) => step.response.isError)).toEqual([false, false, false, false]);
    },
  },
  {
    name: "propose-dismiss",
    description:
      "propose -> dismiss_proposed_action discards without applying; a later confirm_apply is rejected",
    body: async (s) => {
      await s.call("propose_add_custom_filament", FILAMENT);
      const id = s.lastProposedId();
      await s.call("dismiss_proposed_action", { action_id: id });
      await s.call("list_pending_actions");
      await s.call("confirm_apply", { action_id: id });
    },
    assert: (s) => {
      expect(s.pending.size).toBe(0);
      expect(listCustomFilaments(s.dataDir)).toHaveLength(0);
      expect(s.steps.map((step) => step.response.isError)).toEqual([false, false, false, true]);
    },
  },
  {
    name: "confirm-twice",
    description:
      "confirm_apply twice on the same action_id applies once; the second call is rejected without mutating",
    body: async (s) => {
      await s.call("propose_add_custom_filament", FILAMENT);
      const id = s.lastProposedId();
      await s.call("confirm_apply", { action_id: id });
      await s.call("confirm_apply", { action_id: id });
      await s.call("list_pending_actions");
    },
    assert: (s) => {
      expect(s.pending.size).toBe(0);
      expect(listCustomFilaments(s.dataDir)).toHaveLength(1);
      expect(s.steps.map((step) => step.response.isError)).toEqual([false, false, true, false]);
    },
  },
  {
    name: "unknown-key-rejected",
    description:
      "confirm_apply and dismiss_proposed_action reject an action_id that was never proposed; nothing is applied",
    body: async (s) => {
      await s.call("propose_add_custom_filament", FILAMENT);
      await s.call("confirm_apply", { action_id: UNKNOWN_ACTION_ID });
      await s.call("dismiss_proposed_action", { action_id: UNKNOWN_ACTION_ID });
      await s.call("list_pending_actions");
    },
    assert: (s) => {
      expect(s.pending.size).toBe(1);
      expect(listCustomFilaments(s.dataDir)).toHaveLength(0);
      expect(s.steps.map((step) => step.response.isError)).toEqual([false, true, false, false]);
      expect(s.steps[2]?.response.content[0]?.json).toMatchObject({ ok: false });
    },
  },
];

describe("golden MCP proposal lifecycle transcripts", () => {
  const cleanups: (() => Promise<void>)[] = [];
  afterEach(async () => {
    while (cleanups.length) await cleanups.pop()!();
  });

  for (const scenario of SCENARIOS) {
    it(`${scenario.name} matches fixtures/parity/mcp-proposals/${scenario.name}.json`, async () => {
      let session!: Session;
      const run = await runScenario(scenario.name, scenario.description, async (s) => {
        session = s;
        await scenario.body(s);
      });
      cleanups.push(run.cleanup);
      scenario.assert(session);
      expect(JSON.stringify(run.transcript)).not.toContain(run.dataDir);
      checkGolden(scenario.name, run.transcript);
    });
  }

  it("fixture directory holds exactly the golden scenarios", () => {
    const files = readdirSync(FIXTURE_DIR)
      .filter((f) => f.endsWith(".json"))
      .sort();
    expect(files).toEqual(SCENARIOS.map((s) => `${s.name}.json`).sort());
  });

  it("normalization is stable across runs", async () => {
    const [a, b] = await Promise.all(
      [0, 1].map(() =>
        runScenario("stability", "", async (s) => {
          await s.call("propose_add_custom_filament", FILAMENT);
          await s.call("confirm_apply", { action_id: s.lastProposedId() });
        }),
      ),
    );
    cleanups.push(a!.cleanup, b!.cleanup);
    expect(a!.transcript).toEqual(b!.transcript);
  });
});


describe("Build planning proposal parity", () => {
  it("records addon and brief confirmation on the shared seed", async () => {
    const scenario = "build-planning/addon-brief";
    const seed = readFileSync(join(FIXTURE_DIR,"build-planning/seed.sql"),"utf8");
    const run = await runScenario(scenario,"Rust storage addon and brief proposal/confirm identity",async (s) => {
      await s.call("add_addon", {plan_id:999999,source_name:"Addon"});
      await s.call("add_addon", {plan_id:2,source_name:"Addon"});
      await s.call("add_addon", {plan_id:1,source_name:"Addon"});
      await s.call("confirm_apply", {action_id:s.lastProposedId(),suggested_excludes:[" custom.stl "]});
      await s.call("propose_update_build_brief", {plan_id:1,requirements:[{key:" size ",value:" 350 ",status:"satisfied"}]});
      await s.call("confirm_apply", {action_id:s.lastProposedId()});
    },seed);
    try { checkGolden(scenario,run.transcript); } finally { await run.cleanup(); }
  });
});
