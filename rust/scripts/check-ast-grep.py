"""Fail unless Rust ast-grep rule counts equal the checked-in baseline.

The baseline may not rise over origin/main unless PP_AST_GREP_ALLOW_BASELINE_RAISE=1.
"""

from collections import Counter
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


BASE_REF = "origin/main"
RAISE_OVERRIDE = "PP_AST_GREP_ALLOW_BASELINE_RAISE"
PLACEMENT_FILE = "desktop-rebuild-baseline.json"
PLACEMENT_CLASSES = ("module", "item", "statement")
PLACEMENT_RULES = {f"desktop-rebuild-{name}" for name in PLACEMENT_CLASSES}


# Lex independently of the Rust AST: token trees do not expose attribute nodes.
# Keep byte offsets so coverage can be compared even after non-ASCII comments.
RUST_TOKEN = re.compile(
    rb'//[^\n]*|/\*|(?:br|cr|r)(?P<hashes>\#*)".*?"(?P=hashes)'
    rb"|b?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n]|[\xC2-\xF4][\x80-\xBF]+)'"
    rb'|b?"(?:\\.|[^"\\])*"|r\#[A-Za-z_][A-Za-z_0-9]*'
    rb'|[A-Za-z_][A-Za-z_0-9]*|[^\s]', re.DOTALL,
)


def rust_tokens(data):
    tokens = []
    cursor = 0
    while match := RUST_TOKEN.search(data, cursor):
        value = match.group()
        cursor = match.end()
        if value == b"/*":
            depth = 1
            while depth:
                comment = re.search(rb"/\*|\*/", data[cursor:])
                if comment is None:
                    raise ValueError("unterminated Rust block comment")
                depth += 1 if comment.group() == b"/*" else -1
                cursor += comment.end()
        elif not value.startswith(b"//"):
            tokens.append((value, match.start()))
    return tokens


RUST_ESCAPE = re.compile(
    rb'\\(?:x([0-9a-fA-F]{2})|u\{([0-9a-fA-F_]+)\}|(\r?\n\s*)|(.))',
    re.DOTALL,
)


def feature_string(token):
    """Decode Rust feature literals without interpreting strings as source."""
    raw = re.fullmatch(rb'r(?P<h>\#*)"(.*)"(?P=h)', token, re.DOTALL)
    if raw:
        return raw[2]
    if not (token.startswith(b'"') and token.endswith(b'"')):
        return None

    def unescape(match):
        if match[1]:
            return bytes([int(match[1], 16)])
        if match[2]:
            return chr(int(match[2].replace(b"_", b""), 16)).encode()
        if match[3]:
            return b""
        return {b"n": b"\n", b"r": b"\r", b"t": b"\t", b"0": b"\0",
                b"\\": b"\\", b"'": b"'", b'"': b'"'}.get(match[4], match[0])

    return RUST_ESCAPE.sub(unescape, token[1:-1])


def attribute_gate(tokens, start, end, pairs):
    """Inspect a cfg predicate and recursively applied cfg/cfg_attr attributes."""
    name = tokens[start][0].removeprefix(b"r#")
    if (name not in (b"cfg", b"cfg_attr") or start + 1 >= end
            or tokens[start + 1][0] != b"("):
        return False
    close = pairs.get(start + 1)
    if close is None or close >= end:
        return False
    arguments = []
    first = cursor = start + 2
    while cursor < close:
        if tokens[cursor][0] == b",":
            arguments.append((first, cursor))
            first = cursor + 1
        cursor = pairs.get(cursor, cursor) + 1
    arguments.append((first, close))
    predicate_end = close if name == b"cfg" else arguments[0][1]
    if any(feature_string(tokens[i][0]) == b"desktop_rebuild"
           for i in range(start + 2, predicate_end)):
        return True
    return name == b"cfg_attr" and any(
        first < last and attribute_gate(tokens, first, last, pairs)
        for first, last in arguments[1:]
    )


def text_gates(data):
    """Count each outer attachment once, including nested conditional gates."""
    tokens = rust_tokens(data)
    pairs = {}
    stack = []
    for i, (token, _) in enumerate(tokens):
        if token in (b"(", b"[", b"{"):
            stack.append(i)
        elif token in (b")", b"]", b"}") and stack:
            pairs[stack.pop()] = i
    gates = set()
    for i, (value, offset) in enumerate(tokens):
        if value != b"#":
            continue
        bracket = i + 1
        if bracket < len(tokens) and tokens[bracket][0] == b"!":
            bracket += 1
        if bracket not in pairs or tokens[bracket][0] != b"[":
            continue
        if attribute_gate(tokens, bracket + 1, pairs[bracket], pairs):
            gates.add(offset)
    return gates


MACRO_RULE = """id: placement-macro
language: Rust
rule:
  any:
    - kind: macro_invocation
    - kind: macro_definition
"""


def complete_placement_hits(root, hits):
    """Every textual gate must have exactly one class, or be an opaque macro gate."""
    macro_scan = subprocess.run(
        ["ast-grep", "scan", "--inline-rules", MACRO_RULE, "--json=compact", "crates"],
        cwd=root, text=True, capture_output=True, check=True,
    )
    macros = {}
    for hit in json.loads(macro_scan.stdout):
        macros.setdefault(hit["file"], []).append(hit["range"]["byteOffset"])
    candidates = {}
    for hit in hits:
        if hit["ruleId"] in PLACEMENT_RULES:
            candidates.setdefault(hit["file"], {}).setdefault(
                hit["range"]["byteOffset"]["start"], []).append(hit)
    result = [hit for hit in hits if hit["ruleId"] not in PLACEMENT_RULES]
    total = 0
    ast_total = 0
    macro_total = 0
    for path in sorted((root / "crates").rglob("*.rs")):
        file = path.relative_to(root).as_posix()
        data = path.read_bytes()
        gates = text_gates(data)
        total += len(gates)
        unexpected = candidates.get(file, {}).keys() - gates
        if unexpected:
            raise ValueError(f"{file}: AST placement hits have no text gate: {sorted(unexpected)}")
        ast_total += sum(len(group) for group in candidates.get(file, {}).values())
        for offset in sorted(gates):
            classified = candidates.get(file, {}).get(offset, [])
            if not classified and any(span["start"] <= offset < span["end"]
                                      for span in macros.get(file, [])):
                macro_total += 1
                classified = [{
                    "file": file, "ruleId": "desktop-rebuild-statement",
                    "range": {"byteOffset": {"start": offset},
                              "start": {"line": data[:offset].count(b"\n")}},
                }]
            if len(classified) != 1:
                raise ValueError(f"{file}: byte {offset}: placement coverage expected one class, got {len(classified)}")
            result.extend(classified)
    classified_total = ast_total + macro_total
    print(f"desktop-rebuild coverage: {classified_total} classified = {total} text gates")
    return result


def placement_counts(root, hits):
    """Include zero-count crates so the baseline is a complete inventory."""
    counts = {path.name: dict.fromkeys(PLACEMENT_CLASSES, 0)
              for path in sorted((root / "crates").iterdir()) if path.is_dir()}
    for hit in hits:
        if hit["ruleId"] in PLACEMENT_RULES:
            crate = Path(hit["file"]).parts[1]
            counts[crate][hit["ruleId"].removeprefix("desktop-rebuild-")] += 1
    return counts


def parse_placement(text):
    baseline = json.loads(text)
    if not isinstance(baseline, dict):
        raise ValueError("placement baseline must map crates to class counts")
    for counts in baseline.values():
        if (not isinstance(counts, dict) or counts.keys() != set(PLACEMENT_CLASSES)
                or any(type(n) is not int or n < 0 for n in counts.values())):
            raise ValueError("placement baseline needs nonnegative module/item/statement counts")
    return baseline


def check_placement(root, hits):
    baseline = parse_placement((root / PLACEMENT_FILE).read_text())
    counts = placement_counts(root, hits)
    spec = f"{BASE_REF}:./{PLACEMENT_FILE}"
    previous = subprocess.run(["git", "show", spec], cwd=root, text=True, capture_output=True)
    if previous.returncode:
        # Distinguish first landing from a broken reference or git invocation.
        if subprocess.run(["git", "cat-file", "-e", spec], cwd=root, capture_output=True).returncode != 128:
            raise ValueError("could not read placement baseline from main")
        main = {}
        print(f"desktop-rebuild raise check skipped (first landing): {BASE_REF} has no {PLACEMENT_FILE}")
    else:
        main = parse_placement(previous.stdout)
    status = 0
    for crate in sorted(counts.keys() | baseline.keys()):
        actual = counts.get(crate, dict.fromkeys(PLACEMENT_CLASSES, 0))
        print(f"desktop-rebuild {crate}: " + ", ".join(f"{k}={actual[k]}" for k in PLACEMENT_CLASSES))
        if counts.get(crate) != baseline.get(crate):
            print(f"desktop-rebuild {crate}: counts differ from {PLACEMENT_FILE}")
            status = 1
        before = main.get(crate, {}).get("statement", 0)
        if previous.returncode == 0 and baseline.get(crate, {}).get("statement", 0) > before:
            print(f"desktop-rebuild {crate}: statement baseline raised over {BASE_REF} ({before} -> {baseline[crate]['statement']})")
            status = 1
    if status:
        for hit in hits:
            if hit["ruleId"] == "desktop-rebuild-statement":
                print(f'{hit["file"]}:{hit["range"]["start"]["line"] + 1}: statement gate')
    return status



# Keywords have no ordinary identifier spelling. Replacing their raw prefix
# would change the parse, whereas non-keyword raw identifiers name the same item.
RUST_KEYWORDS = set("""
    as async await break const continue crate dyn else enum extern false fn for
    if impl in let loop match mod move mut pub ref return self Self static struct
    super trait true type unsafe use where while abstract become box do final
    gen macro override priv try typeof unsized virtual yield
""".split())
RAW_IDENTIFIER_RULE = """id: raw-identifier
language: Rust
rule:
  any:
    - kind: identifier
    - kind: type_identifier
  regex: '^r#'
"""


def command_hits_with_canonical_names(root, original_hits):
    """Canonicalize AST identifier nodes in a scan-only copy, keeping byte spans.

    Repeated YAML metavariables compare spelling, but Rust resolves r#p and p
    to the same name. The AST pass excludes strings and comments. Only affected
    files are rescanned, with the same crate-relative paths and exemptions.
    Other rules retain their original scan results.
    """
    raw_scan = subprocess.run(
        ["ast-grep", "scan", "--inline-rules", RAW_IDENTIFIER_RULE,
         "--json=compact", "crates"], cwd=root, text=True,
        capture_output=True, check=True,
    )
    edits = {}
    for node in json.loads(raw_scan.stdout):
        if node["text"][2:] not in RUST_KEYWORDS:
            edits.setdefault(node["file"], []).append(node["range"]["byteOffset"])
    if not edits:
        return original_hits
    with tempfile.TemporaryDirectory(prefix="pp-ast-grep-names-") as directory:
        copy = Path(directory)
        shutil.copy(root / "sgconfig.yml", copy)
        shutil.copytree(root / "rules", copy / "rules")
        for file, ranges in edits.items():
            relative = Path(file)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError(f"unexpected scanner path: {file}")
            data = bytearray((root / relative).read_bytes())
            for span in ranges:
                start = span["start"]
                if data[start:start + 2] != b"r#":
                    raise ValueError(f"invalid raw identifier span: {file}")
                data[start:start + 2] = b"  "
            target = copy / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        scan = subprocess.run(
            ["ast-grep", "scan", "--config", "sgconfig.yml", "--json=compact", "crates"],
            cwd=copy, text=True, capture_output=True, check=True,
        )
        hits = [hit for hit in original_hits
                if hit["ruleId"] != "runtime-process-command" or hit["file"] not in edits]
        for hit in json.loads(scan.stdout):
            if hit["ruleId"] == "runtime-process-command":
                # Report the original spelling; normalized spans have equal length.
                span = hit["range"]["byteOffset"]
                original = (root / hit["file"]).read_bytes()
                # The parser starts the canonical identifier after the two spaces.
                if span["start"] >= 2 and original[span["start"] - 2:span["start"]] == b"r#":
                    span["start"] -= 2
                    hit["range"]["start"]["column"] -= 2
                hit["text"] = original[span["start"]:span["end"]].decode()
                hits.append(hit)
        return hits


def parse_baseline(text):
    baseline = json.loads(text)
    if not isinstance(baseline, dict) or not baseline or any(type(value) is not int or value < 0 for value in baseline.values()):
        raise ValueError("ast-grep baseline must contain nonnegative integer counts")
    return baseline


def main_baseline(root):
    """Return the baseline at BASE_REF, or None before it first lands there.

    A missing BASE_REF is an error, so an unfetched checkout cannot skip the rise check.
    """
    if subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", BASE_REF + "^{commit}"],
        cwd=root, capture_output=True,
    ).returncode:
        raise ValueError(f"{BASE_REF} is unavailable; fetch it so baseline raises can be checked")
    spec = f"{BASE_REF}:./ast-grep-baseline.json"
    if subprocess.run(["git", "cat-file", "-e", spec], cwd=root, capture_output=True).returncode:
        return None
    shown = subprocess.run(["git", "show", spec], cwd=root, text=True, capture_output=True, check=True)
    return parse_baseline(shown.stdout)


def raised_over_main(baseline, main):
    """Map each rule whose baseline rose to (main, local). Rules absent from main count as zero."""
    return {
        rule: (main.get(rule, 0), allowed)
        for rule, allowed in baseline.items()
        if allowed > main.get(rule, 0)
    }


def main():
    root = Path(__file__).resolve().parents[1]
    baseline = parse_baseline((root / "ast-grep-baseline.json").read_text())
    main = main_baseline(root)
    scan = subprocess.run(
        ["ast-grep", "scan", "--config", "sgconfig.yml", "--json=compact", "--inspect=entity", "crates"],
        cwd=root,
        text=True,
        capture_output=True,
        check=True,
    )
    # Use the pinned scanner's loaded-rule report, including rules with no hits.
    # A deleted, unconfigured, or disabled rule must not look like a decrease.
    loaded = {
        rule for rule, severity in re.findall(
            r"^sg: entity\|rule\|(.+): finalSeverity=(\w+)$", scan.stderr, re.MULTILINE
        ) if severity != "Off"
    }
    if loaded != baseline.keys() | PLACEMENT_RULES:
        raise ValueError(
            f"ast-grep rule set differs from baseline: "
            f"unloaded={sorted((baseline.keys() | PLACEMENT_RULES) - loaded)}, "
            f"unbaselined={sorted(loaded - (baseline.keys() | PLACEMENT_RULES))}"
        )
    hits = command_hits_with_canonical_names(root, json.loads(scan.stdout))
    hits = complete_placement_hits(root, hits)
    placement_status = check_placement(root, hits)
    hits = [hit for hit in hits if hit["ruleId"] not in PLACEMENT_RULES]
    counts = Counter(hit["ruleId"] for hit in hits)
    unknown = counts.keys() - baseline.keys()
    if unknown:
        raise ValueError(f"rules missing from ast-grep baseline: {sorted(unknown)}")
    exceeded = {rule for rule, allowed in baseline.items() if counts[rule] > allowed}
    lowered = {rule for rule, allowed in baseline.items() if counts[rule] < allowed}
    raised = {} if main is None else raised_over_main(baseline, main)
    allow_raise = os.environ.get(RAISE_OVERRIDE) == "1"
    for rule, allowed in baseline.items():
        print(f"{rule}: {counts[rule]} hits (baseline {allowed})")
    for hit in hits:
        if hit["ruleId"] in exceeded:
            line = hit["range"]["start"]["line"] + 1
            print(f'{hit["file"]}:{line}: {hit["ruleId"]}: {hit["text"]}')
    if main is None:
        print(f"ast-grep baseline raise check skipped: {BASE_REF} has no ast-grep-baseline.json yet (first landing).")
    for rule, (before, after) in raised.items():
        print(f"{rule}: baseline raised {before} -> {after} over {BASE_REF}")
    status = placement_status
    if exceeded:
        print("ast-grep ratchet FAILED: hits exceed the baseline.")
        status = 1
    if lowered:
        print(f"ast-grep ratchet FAILED: baseline must equal hit counts; lower ast-grep-baseline.json for {sorted(lowered)}.")
        status = 1
    if raised and not allow_raise:
        print(f"ast-grep ratchet FAILED: baseline raised over {BASE_REF}; set {RAISE_OVERRIDE}=1 only for an intentional raise.")
        status = 1
    if status == 0:
        print("ast-grep ratchet passed: counts match the baseline.")
    return status


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"ast-grep ratchet error: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr, file=sys.stderr)
        sys.exit(2)
