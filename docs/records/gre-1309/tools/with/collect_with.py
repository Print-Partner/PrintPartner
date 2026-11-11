"""Collect t3_thread_read results for the GRE-1309 with-arm threads.

Adapted from gre-1309-logs/tools-rerun/collect_rerun.py. Selects threads by
thread_id from manifest.tsv (not by title). Writes <session>.json per thread,
answers.tsv, and prints per-thread status.

usage: collect_with.py SESSION_DIR MANIFEST OUT_DIR
"""
import csv, glob, json, os, re, sys

D, MANIFEST, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
TOOL = "mcp__t3-code__t3_thread_read"
with open(MANIFEST) as f:
    rows = list(csv.DictReader(f, delimiter="\t"))
SESSION_BY_TID = {r["thread_id"]: r["session"] for r in rows}
assert len(SESSION_BY_TID) == 30, len(SESSION_BY_TID)

calls = []
transcripts = [(os.path.basename(p)[6:-6], p) for p in sorted(glob.glob(D + "/subagents/agent-*.jsonl"))]
transcripts.append(("main", D + ".jsonl"))
for agent, path in transcripts:
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
        if inp.get("threadId", "") in SESSION_BY_TID or inp.get("itemId"):
            failures.append((agent, inp.get("threadId", "")[-24:], inp.get("itemId", ""), inp.get("textOffset"), txt[:160]))
        continue
    tid = d.get("thread", {}).get("threadId", "")
    if tid not in SESSION_BY_TID:
        continue
    if inp.get("itemId"):
        for it in d.get("items", []):
            if it["itemId"] == inp["itemId"]:
                slot = chunks.setdefault(it["itemId"], {})
                off = inp.get("textOffset", 0)
                if off not in slot or len(it["text"]) > len(slot[off][0]["text"]):
                    slot[off] = (it, f"{agent}:{src}")
    else:
        # The transcript can record one tool result twice; keep one copy of each identical page.
        seen = pages.setdefault(tid, [])
        if not any(json.dumps(x[0], sort_keys=True) == json.dumps(d, sort_keys=True) for x in seen):
            seen.append((d, f"{agent}:{src}"))

os.makedirs(OUT, exist_ok=True)
for agent, tid, item, off, msg in failures:
    print("FAILED CALL", agent, tid, item[-8:], off, repr(msg), file=sys.stderr)
status = []
answers = []
for tid, ps in pages.items():
    session = SESSION_BY_TID[tid]
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
    gaps = [p for p in range(max(items) + 1) if p not in items] if items else []
    th = ps[0][0]["thread"]
    rec = {"thread": th, "recentRuns": ps[0][0].get("recentRuns"), "items": merged,
           "complete": has_more_false and not gaps and not notes, "notes": notes,
           "sources": sorted({s for _, s in ps} | {s for c in chunks.values() for (it, s) in c.values() if it["sourceThreadId"] == tid}),
           "rawPages": [d for d, _ in ps]}
    with open(os.path.join(OUT, f"{session}.json"), "w") as f:
        json.dump(rec, f, indent=1)
    msgs = [it for it in merged if it["type"] == "assistant_message"]
    last = msgs[-1]["text"] if msgs else ""
    answers.append((session, " ".join(last.split())))
    status.append((session, len(merged), th["itemCount"], has_more_false, gaps, len(notes), sorted(SESSION_BY_TID[t] for t in []) or ""))

missing = sorted(set(SESSION_BY_TID.values()) - {s for s, *_ in status})
with open(os.path.join(OUT, "answers.tsv"), "w") as f:
    f.write("session\tanswer\n")
    for s, a in sorted(answers):
        f.write(f"{s}\t{a}\n")
for r in sorted(status):
    print(*r[:6], sep=" | ")
print("threads read:", len(status), "missing:", missing, "calls:", len(calls), "failed:", len(failures))
