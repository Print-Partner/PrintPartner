import { mkdirSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { createSelfHostPorts } from "../adapters/self-host/index.js";
import { classifyDocPath } from "../services/github-sync.js";
import {
  buildKnowledgeBundle,
  importKnowledgeBundle,
  parseKnowledgeBundle,
} from "../services/knowledge-bundle.js";
import { walkSourceDocs, keywordFilterScore } from "../services/source-docs-scan.js";
import { indexSourceDocsFromDisk } from "../services/source-docs-index.js";
import { gatherSourceDocsForAssistant } from "../assistant/source-docs-digest.js";
import { invokeAssistantTool } from "../assistant/tools.js";
import { loadConfig } from "../config.js";

describe("source docs intelligence", () => {
  let dataDir: string;
  let repo: NonNullable<ReturnType<typeof createSelfHostPorts>["repository"]>;

  beforeEach(async () => {
    dataDir = mkdtempSync(join(tmpdir(), "pp-docs-"));
    const ports = createSelfHostPorts(dataDir);
    await ports.db.connect();
    repo = ports.repository!;
  });

  afterEach(() => {
    rmSync(dataDir, { recursive: true, force: true });
  });

  it("classifyDocPath recognizes readme, md, and pdf", () => {
    expect(classifyDocPath("README.md")).toBe("readme");
    expect(classifyDocPath("docs/assembly.md")).toBe("md");
    expect(classifyDocPath("Manual.pdf")).toBe("pdf");
    expect(classifyDocPath("part.stl")).toBeNull();
  });

  it("SOURCE_DOCS_MAX_BYTES defaults to ~1 GiB", () => {
    // Covered in config.test.ts with proper env restore. Keep this file focused
    // on source-docs indexing/assistant behaviour so full-suite runs do not
    // leave SOURCE_DOCS_MAX_BYTES mutated if an assertion fails mid-test.
    expect(loadConfig().sourceDocsMaxBytes).toBeGreaterThan(0);
  });

  it("indexes markdown from disk into source_docs and lists via repo", () => {
    const source = repo.createSource({
      name: "DocsKit",
      url: "https://github.com/example/docs",
      source_kind: "github",
    });
    const root = source.local_path!;
    mkdirSync(join(root, "docs"), { recursive: true });
    writeFileSync(join(root, "README.md"), "# Hello\n\nBuild notes.", "utf8");
    writeFileSync(join(root, "docs", "guide.md"), "Guide body", "utf8");

    const indexed = indexSourceDocsFromDisk(repo, source.id, root);
    expect(indexed.doc_count).toBe(2);
    const docs = repo.listSourceDocs(source.id);
    expect(docs.some((d) => d.kind === "readme")).toBe(true);
    expect(repo.getSource(source.id)?.doc_count).toBe(2);

    const walked = walkSourceDocs(root);
    expect(walked.map((d) => d.path).sort()).toEqual(["README.md", "docs/guide.md"].sort());
  });

  it("notes CRUD + knowledge bundle round-trip", () => {
    const source = repo.createSource({
      name: "NotesKit",
      url: "https://github.com/example/notes",
      source_kind: "github",
    });
    const plan = repo.createProfile("Plan A", source.id);
    const note = repo.createSourceNote({
      projectId: source.id,
      profileId: plan.id,
      title: "Tap height",
      bodyMarkdown: "Set probe Z to -1.2",
    });
    expect(repo.listSourceNotes(source.id)).toHaveLength(1);

    const bundle = buildKnowledgeBundle(repo, source.id);
    expect(bundle.format).toBe("print-partner-knowledge");
    expect(bundle.notes).toHaveLength(1);

    const parsed = parseKnowledgeBundle(JSON.parse(JSON.stringify(bundle)));
    const other = repo.createSource({
      name: "ImportTarget",
      url: "https://github.com/example/import",
      source_kind: "github",
    });
    const result = importKnowledgeBundle(repo, other.id, parsed);
    expect(result.notes_imported).toBe(1);
    expect(repo.listSourceNotes(other.id)[0]?.body_markdown).toContain("probe Z");

    repo.deleteSourceNote(note.id);
    expect(repo.listSourceNotes(source.id)).toHaveLength(0);
  });

  it("keywordFilterScore and get_source_docs tool", async () => {
    expect(keywordFilterScore("Voron tap probe", "tap")).toBeGreaterThan(0);
    expect(keywordFilterScore("hello", "tap")).toBe(0);

    const source = repo.createSource({
      name: "Tap",
      url: "https://github.com/example/tap",
      source_kind: "github",
    });
    const root = source.local_path!;
    mkdirSync(root, { recursive: true });
    writeFileSync(join(root, "README.md"), "Tap is a Z probe for Voron.", "utf8");
    indexSourceDocsFromDisk(repo, source.id, root);
    repo.createSourceNote({
      projectId: source.id,
      bodyMarkdown: "User tip: mount tap on toolhead",
      title: "Mount",
    });

    const gathered = await gatherSourceDocsForAssistant({
      repo,
      sourceId: source.id,
      query: "probe",
      includeLiveReadme: false,
    });
    expect("error" in gathered).toBe(false);
    if (!("error" in gathered)) {
      expect(gathered.untrusted).toBe(true);
      expect(gathered.docs.length + gathered.notes.length).toBeGreaterThan(0);
    }

    const tool = await invokeAssistantTool(
      "get_source_docs",
      { source_name: "Tap", query: "probe" },
      { repo },
    );
    const parsed = JSON.parse(tool.content);
    expect(parsed.warning).toMatch(/UNTRUSTED/i);
    expect(parsed.buckets).toMatchObject({
      synced_docs: expect.any(Number),
      advisor_notes: expect.any(Number),
      live_readme: expect.any(Number),
      pdf_pending: expect.any(Number),
    });
  }, 15000);

  it("get_source_docs returns hint when docs empty but Source notes exist", async () => {
    const source = repo.createSource({
      name: "NotesOnly",
      url: "https://github.com/example/notes-only",
      source_kind: "github",
    });
    repo.createSourceNote({
      projectId: source.id,
      title: "Advisor: Workflow",
      bodyMarkdown: "Sync then attach as base.",
    });

    const gathered = await gatherSourceDocsForAssistant({
      repo,
      sourceId: source.id,
      includeLiveReadme: false,
    });
    expect("error" in gathered).toBe(false);
    if (!("error" in gathered)) {
      expect(gathered.buckets.synced_docs).toBe(0);
      expect(gathered.buckets.advisor_notes).toBe(1);
      expect(gathered.notes[0]?.bucket).toBe("advisor_notes");
      expect(gathered.hint).toMatch(/Sync/i);
      expect(gathered.hint).toMatch(/Source notes/i);
    }
  });
});
