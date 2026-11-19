#!/usr/bin/env python3
"""Static guard: in the reader's SQL, `$.text` appears only as the whole argument of t3_ask_count.

Message text may reach the program in one place only: inside the SQL function that counts asks
and returns a number. Anything else that names a `text` key in a JSON path is a way for message
text to leave the database, so it is a violation.

Allowed, exactly:
    t3_ask_count(json_extract(<column>, '$.text'))
    t3_ask_count(<column> ->> '$.text')
Not allowed: wrapping it (substr, COALESCE, lower, CAST), a second argument, selecting it, any other
path that ends in `.text`, a path built at run time, or a bound path parameter.

The guard is checked three ways so it cannot pass by being blind:
  1. controls: fixtures that must be flagged are flagged, fixtures that must pass are clean;
  2. the real source with a violation injected is flagged, and the same text without it is clean;
  3. on the real tree the scanner has to find every `json_extract(` call it was given (it parses
     what it claims to scan), and it reports how many allowed sites exist. The positive path must also exist.

    python3 tests/scripts/asks_sql_guard.py            run the gates
    python3 tests/scripts/asks_sql_guard.py --list     print every json path the scanner saw
"""
import re
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import asks_lib as lib  # noqa: E402

# A JSON path whose last key is `text`: $.text, $.a.text, $[0].text, $."text", $.a."text"
TEXT_PATH = re.compile(r"""\$(?:\.[A-Za-z_][A-Za-z0-9_]*|\."[^"]*"|\[[^\]]*\])*\.\s*"?text"?(?![A-Za-z0-9_])""")
# The arrow operators and string joins that name the key without a `$.` path.
ARROW_TEXT = re.compile(r"""->>?\s*'text'|\|\|\s*'text'""")
# A path assembled at run time from Rust (`'$.{k}'`, `'$.'||x`). The keys come from literal lists,
# so each site is checked against the string literals around it and counted against REVIEWED_DYNAMIC.
FORMAT_PATH = re.compile(r"""\$\.\{|'\$\.'\s*\|\||\$\.\s*"\s*\+""")
# A path bound as a parameter: nothing static says what it names, so it is never allowed.
BOUND_PATH = re.compile(r"""jsonb?_extract\(\s*[^,()]+,\s*(?:\?|[:@$][A-Za-z_])|->>?\s*\?""")
# Reviewed 2026-11-18: each builds its keys from static lists of literals (projection field
# allowlists) that do not contain "text". A new or moved site must be reviewed and added here.
REVIEWED_DYNAMIC = {"crates/core/src/bench.rs": 3, "crates/core/src/recap.rs": 2, "crates/core/src/source.rs": 3}
COLUMN = r"[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)?"
ALLOWED = re.compile(
    r"t3_ask_count\(\s*(?:jsonb?_extract\(\s*" + COLUMN + r"\s*,\s*'(\$\.text)'\s*\)|" + COLUMN + r"\s*->>\s*'(\$\.text)')\s*\)")
JSON_EXTRACT_ARGS = re.compile(r"json_extract\(\s*[^,()]+,\s*'([^']*)'\s*\)")
REGISTERED = re.compile(r'create_scalar_function\(\s*"t3_ask_count"')


def line_of(text, pos):
    return text.count("\n", 0, pos) + 1


def literals_before(text, pos):
    """String literals from the start of the enclosing function to `pos`: what feeds a built path."""
    starts = [m.start() for m in re.finditer(r"\bfn\s+\w+", text[:pos])]
    window = text[starts[-1] if starts else 0:pos]
    return re.findall(r'"((?:[^"\\]|\\.)*)"', window)


# Inspect each SELECT whose own FROM names the message table. Limit the check to
# its projection, so payload access in predicates and unrelated tables stays valid.
MESSAGE_SELECT = re.compile(r"\bSELECT\b(?P<projection>(?:(?!\bSELECT\b|\bFROM\b).)*)\bFROM\s+(?:\{PREFIX\}|\{P\}|orchestration_v2_projection_)messages(?:\s+(?:AS\s+)?(?P<alias>\w+))?", re.I | re.S)
MESSAGE_OUTPUT = re.compile(r"(?<![\w])(?:[\w]+\s*\.\s*)?(?:payload_json|body|text|\*)(?![\w])", re.I)

def scan(files, reviewed=REVIEWED_DYNAMIC):
    """files: {name: source text}. Returns (violations, allowed_sites, json_extract_calls, json_paths_parsed)."""
    violations, allowed, calls, parsed = [], [], 0, 0
    for name, text in sorted(files.items()):
        ok = [m.span(1) if m.group(1) else m.span(2) for m in ALLOWED.finditer(text)]
        allowed += [(name, line_of(text, a)) for a, _ in ok]
        for pattern, kind in ((TEXT_PATH, "text path"), (ARROW_TEXT, "text key by operator"), (BOUND_PATH, "bound path")):
            for m in pattern.finditer(text):
                inside = kind == "text path" and any(a <= m.start() and m.end() <= b for a, b in ok)
                if not inside:
                    violations.append((name, line_of(text, m.start()), kind, m.group(0)))
        for query in MESSAGE_SELECT.finditer(text):
            projection = query.group("projection")
            # A scalar count is the only approved projection of message text.
            projection = ALLOWED.sub("0", projection)
            projection = re.sub(r"json_valid\(\s*\w+\.payload_json\s*\)|json_extract\(\s*\w+\.payload_json\s*,\s*'\$\.phase'\s*\)", "0", projection, flags=re.I)
            for leak in MESSAGE_OUTPUT.finditer(projection):
                violations.append((name, line_of(text, query.start("projection") + leak.start()),
                                   "message projection", leak.group()))
        dynamic = list(FORMAT_PATH.finditer(text))
        for m in dynamic:
            if "text" in literals_before(text, m.start()):
                violations.append((name, line_of(text, m.start()), "built path fed a \"text\" key", m.group(0)))
        if len(dynamic) > reviewed.get(name, 0):
            violations.append((name, line_of(text, dynamic[reviewed.get(name, 0)].start()), "unreviewed built path", dynamic[reviewed.get(name, 0)].group(0)))
        calls += text.count("json_extract(")
        parsed += len(JSON_EXTRACT_ARGS.findall(text))
    return violations, allowed, calls, parsed


def tree():
    """The reader's sources: every Rust file under crates/*/src."""
    return {str(p.relative_to(REPO)): p.read_text() for p in sorted(REPO.glob("crates/*/src/**/*.rs"))}


# ── fixtures for the controls ──────────────────────────────────────────────

MUST_PASS = {
    "unrelated payload": "SELECT m.payload_json FROM other m",
    "jsonb scalar": "SELECT t3_ask_count(jsonb_extract(m.payload_json,'$.text')) FROM m",
    "plain": "SELECT t3_ask_count(json_extract(payload_json,'$.text')) AS asks FROM m",
    "qualified and spaced": "SELECT t3_ask_count(  json_extract( m.payload_json , '$.text' )  ) FROM m",
    "multiline": "SELECT t3_ask_count(\n  json_extract(\n    payload_json,\n    '$.text'\n  )\n) FROM m",
    "arrow": "SELECT t3_ask_count(payload_json ->> '$.text') FROM m",
    "similar keys": "SELECT json_extract(payload_json,'$.textOmitted'), json_extract(payload_json,'$.context'),"
                    " json_extract(payload_json,'$.texture') FROM m",
    "no text at all": "SELECT json_extract(payload_json,'$.status') FROM runs",
}
MUST_FLAG = {  # name: (sql, violations expected)
    "payload projection": ("SELECT m.payload_json FROM orchestration_v2_projection_messages m", 1),
    "report table prefix": ("SELECT m.payload_json FROM {P}messages m", 1),
    "wildcard projection": ("SELECT m.* FROM orchestration_v2_projection_messages m", 1),
    "bound jsonb path": ("SELECT jsonb_extract(m.payload_json, ?1) FROM other m", 1),
    "selected": ("SELECT json_extract(payload_json,'$.text') AS body FROM m", 1),
    "substr": ("SELECT substr(json_extract(payload_json,'$.text'),1,80) FROM m", 1),
    "wrapped in the function": ("SELECT t3_ask_count(substr(json_extract(payload_json,'$.text'),1,9999)) FROM m", 1),
    "coalesce inside": ("SELECT t3_ask_count(COALESCE(json_extract(payload_json,'$.text'),'')) FROM m", 1),
    "second argument": ("SELECT t3_ask_count(json_extract(payload_json,'$.text'), 1) FROM m", 1),
    "function used as a wrapper": ("SELECT lower(t3_ask_count(json_extract(payload_json,'$.text'))||json_extract(payload_json,'$.text')) FROM m", 1),
    "nested path": ("SELECT json_extract(payload_json,'$.message.text') FROM m", 1),
    "array path": ("SELECT json_extract(payload_json,'$.parts[0].text') FROM m", 1),
    "quoted key": ("""SELECT json_extract(payload_json,'$."text"') FROM m""", 1),
    "arrow selected": ("SELECT payload_json ->> '$.text' FROM m", 1),
    "arrow key only": ("SELECT payload_json ->> 'text' FROM m", 1),
    "joined at run time": ("SELECT json_extract(payload_json,'$.'||'text') FROM m", 2),
    "bound path": ("SELECT json_extract(payload_json, ?1) FROM m", 1),
    "formatted path": ('format!("SELECT json_extract(payload_json,\'$.{key}\') FROM m")', 1),
    "in a where clause": ("SELECT 1 FROM m WHERE json_extract(payload_json,'$.text') LIKE '%?%'", 1),
    "one good one bad": ("SELECT t3_ask_count(json_extract(payload_json,'$.text')), json_extract(payload_json,'$.text') FROM m", 1),
    "in a comment": ("// reads $.text for the body\nSELECT 1", 1),
}


class Guard(unittest.TestCase):
    def test_controls_pass_clean_and_count_their_site(self):
        for name, sql in MUST_PASS.items():
            with self.subTest(name):
                v, allowed, _, _ = scan({"fixture": sql})
                self.assertEqual(v, [], f"false positive on {name!r}")
                self.assertEqual(len(allowed), 1 if "t3_ask_count" in sql else 0)

    def test_controls_are_flagged(self):
        for name, (sql, expected) in MUST_FLAG.items():
            with self.subTest(name):
                v, _, _, _ = scan({"fixture": sql})
                self.assertEqual(len(v), expected, f"{name!r}: expected {expected} violation(s), got {v}")

    def test_negative_control_on_the_real_source(self):
        """The real files, scanned as they are, then with one violation appended: the scan must flip."""
        files = tree()
        self.assertTrue(files, "no Rust source found under crates/*/src")
        base, _, _, _ = scan(files)
        self.assertEqual(base, [], "the real source already violates the guard (see test_real_tree)")
        victim = "crates/core/src/source.rs"
        self.assertIn(victim, files)
        for label, injected, expected in (
            ("selected text", 'const LEAK: &str = "SELECT json_extract(payload_json,\'$.text\') AS leaked FROM x";', 1),
            ("wrapped function", 'const LEAK: &str = "SELECT t3_ask_count(substr(json_extract(payload_json,\'$.text\'),1,9)) FROM x";', 1),
            ("new built path", 'fn leak(k: &str) -> String { format!("SELECT json_extract(payload_json,\'$.{k}\') FROM x") }', 1),
        ):
            with self.subTest(label):
                bad = dict(files)
                bad[victim] = files[victim] + "\n" + injected + "\n"
                v, _, _, _ = scan(bad)
                self.assertEqual([x[:2] for x in v], [(victim, bad[victim].count("\n"))],
                                 "the injected line must be the one flagged, at its own line")
                self.assertEqual(len(v), expected)
        # and the allowed form, injected, is clean and counted
        good = dict(files)
        good[victim] = files[victim] + "\nconst OK: &str = \"SELECT t3_ask_count(json_extract(payload_json,'$.text')) FROM x\";\n"
        v, allowed, _, _ = scan(good)
        self.assertEqual(v, [])
        self.assertEqual(len(allowed), len(scan(files)[1]) + 1)

    def test_scanner_reads_what_it_claims_to(self):
        """Guard against a vacuous pass: the scan must cover the real reader and parse every json_extract call."""
        files = tree()
        self.assertGreaterEqual(len(files), 10, "too few source files scanned; a wrong root or glob would pass everything")
        for needed in ("crates/core/src/source.rs", "crates/core/src/live.rs", "crates/core/src/recap.rs"):
            self.assertIn(needed, files)
        _, _, calls, parsed = scan(files)
        self.assertGreaterEqual(calls, 20, "expected the reader's many json_extract calls")
        self.assertEqual(parsed, calls, "a json_extract call the scanner could not parse is a call it cannot check")
        joined = "\n".join(files.values())
        for known in ("$.startedAt", "$.toolName", "$.responseCapability.type"):
            self.assertIn(known, joined)
            self.assertIn(known, "".join(JSON_EXTRACT_ARGS.findall(joined)) or "".join(re.findall(r"'(\$\.[^']+)'", joined)))

    def test_real_tree(self):
        files = tree()
        violations, allowed, _, _ = scan(files)
        self.assertEqual(violations, [], "message text is named outside t3_ask_count:\n" +
                         "\n".join(f"  {n}:{l}: {k}: {s}" for n, l, k, s in violations))
        registered = any(REGISTERED.search(t) for t in files.values())
        if allowed and not registered:
            self.fail("SQL calls t3_ask_count but no create_scalar_function(\"t3_ask_count\") exists: the query would fail at run time")
        print(f"\n  scanned {len(files)} files; {len(allowed)} allowed t3_ask_count($.text) site(s); registered={registered}")

    def test_classifier_is_wired(self):
        """The reader registers and calls the classifier."""
        files = tree()
        _, allowed, _, _ = scan(files)
        registered = any(REGISTERED.search(t) for t in files.values())
        self.assertTrue(registered, "t3_ask_count is used in SQL but not registered")
        self.assertTrue(allowed, "t3_ask_count is registered but no reader query passes it message text")


def listing():
    for name, text in tree().items():
        for m in JSON_EXTRACT_ARGS.finditer(text):
            print(f"{name}:{line_of(text, m.start())}: {m.group(1)}")


if __name__ == "__main__":
    if "--list" in sys.argv:
        listing()
    else:
        lib_main = unittest.main(exit=False, verbosity=2)
        r = lib_main.result
        pend = [1 for _, why in r.skipped if str(why).startswith("PENDING")]
        print(f"\nasks_sql_guard: {r.testsRun - len(r.skipped)} ran, {len(pend)} pending, {len(r.failures)} failed, {len(r.errors)} errors")
        sys.exit(0 if r.wasSuccessful() else 1)
