"""Collect raw t3_thread_read activity results for the GRE-1309 nav-without threads.

Reads tool_use/tool_result pairs from the main and subagent transcripts. A result
the harness saved to tool-results/ is loaded from that file. Page reads are merged
by item position; itemId reads (textOffset chunks) are stitched onto the item text.

usage: collect.py SESSION_DIR OUT_DIR [EXCLUDED_AGENT_IDS]
"""
import glob, json, os, re, sys

D, OUT = sys.argv[1], sys.argv[2]
EXCLUDE = set(filter(None, (sys.argv[3] if len(sys.argv) > 3 else "").split(",")))
TOOL = "mcp__t3-code__t3_thread_read"

calls = []  # (agent, input, result_text, source)
transcripts = [(os.path.basename(p)[6:-6], p) for p in sorted(glob.glob(D + "/subagents/agent-*.jsonl"))]
transcripts.append(("main", D + ".jsonl"))
for agent, path in transcripts:
    if agent in EXCLUDE:
        continue
    uses = {}
    for line in open(path):
        rec = json.loads(line)
        content = (rec.get("message") or {}).get("content")
        if not isinstance(content, list):
            continue
        for b in content:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "tool_use" and b.get("name") == TOOL:
                uses[b["id"]] = b["input"]
            elif b.get("type") == "tool_result" and b.get("tool_use_id") in uses:
                c = b.get("content")
                texts = [c] if isinstance(c, str) else [x["text"] for x in c or [] if x.get("type") == "text"]
                txt = "\n".join(texts)
                m = re.search(r"(/home/\S+?/tool-results/[\w.\-]+\.txt)", txt)
                if not txt.lstrip().startswith("{") and m:
                    calls.append((agent, uses[b["tool_use_id"]], open(m.group(1)).read(), "file:" + os.path.basename(m.group(1))))
                else:
                    calls.append((agent, uses[b["tool_use_id"]], txt, "inline"))

pages, chunks, failures = {}, {}, []
for agent, inp, txt, src in calls:
    try:
        d = json.loads(txt)
    except ValueError:
        failures.append((agent, inp.get("threadId", "")[-24:], inp.get("itemId", ""), inp.get("textOffset"), txt[:160]))
        continue
    tid = d.get("thread", {}).get("threadId", "")
    if "nav-without" not in tid:
        continue
    if inp.get("itemId"):
        for it in d.get("items", []):
            if it["itemId"] == inp["itemId"]:
                slot = chunks.setdefault(it["itemId"], {})
                off = inp.get("textOffset", 0)
                # Keep the longest read at each offset (a 1-char probe must not replace a full chunk).
                if off not in slot or len(it["text"]) > len(slot[off][0]["text"]):
                    slot[off] = (it, f"{agent}:{src}")
    else:
        pages.setdefault(tid, []).append((d, f"{agent}:{src}"))

os.makedirs(OUT, exist_ok=True)
for agent, tid, item, off, msg in failures:
    print("FAILED CALL", agent, tid, item[-8:], off, repr(msg), file=sys.stderr)
rows = []
for tid, ps in pages.items():
    items = {}
    for d, _ in ps:
        for it in d.get("items", []):
            items[it["position"]] = dict(it)
    has_more_false = any(d.get("hasMore") is False for d, _ in ps)
    notes = []
    for pos, it in items.items():
        if not it.get("textTruncated"):
            continue
        text, off = it["text"], it["nextTextOffset"]
        got = chunks.get(it["itemId"], {})
        while off in got:
            ch, _ = got[off]
            text += ch["text"]
            if not ch.get("textTruncated"):
                off = None
                break
            off = ch["nextTextOffset"]
        it["text"] = text
        it["textTruncated"] = off is not None
        it["nextTextOffset"] = off
        it["stitchedChunkOffsets"] = sorted(got)
        if off is not None:
            notes.append(f"pos {pos} text recovered to char {off}, rest not fetched")
    merged = [items[p] for p in sorted(items)]
    gaps = [p for p in range(max(items) + 1) if p not in items]
    th = ps[0][0]["thread"]
    slug = th["title"].replace("nav without ", "").replace(" ", "-")
    rec = {"thread": th, "recentRuns": ps[0][0].get("recentRuns"), "items": merged,
           "complete": has_more_false and not gaps and not notes, "notes": notes,
           "sources": sorted({s for _, s in ps} | {s for c in chunks.values() for (it, s) in c.values() if it["sourceThreadId"] == tid}),
           "rawPages": [d for d, _ in ps]}
    with open(os.path.join(OUT, f"session-{slug}.json"), "w") as f:
        json.dump(rec, f, indent=1)
    rows.append((th["title"], len(merged), th["itemCount"], has_more_false, gaps, notes))
for r in sorted(rows):
    print(*r, sep=" | ")
print("threads:", len(rows), "calls:", len(calls), "failed:", len(failures))
