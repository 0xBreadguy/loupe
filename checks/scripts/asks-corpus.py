#!/usr/bin/env python3
"""The asks-count corpus: made-up messages, and the count the prototype gives each.

`t3_agents_core::asks::count_asks(text)` answers one number for one message: the
questions and statements put to the person in it. The specification is the prototype
`asks/questions.py` from the Insights thread (2026-11-18), whose `asks(text)` returns
`(questions, statements)`; the count is their sum. The corpus in
`checks/fixtures/asks-counts.json` records, for every case, all three numbers, so the
product can be held to the reference without Python, Node or the prototype present.

The texts are made up. They cover the 24 examples that prototype ships, plus
positive and negative asks, markdown headings, labels and lists, self-answered
questions, code, quotes and tables, hard-wrapped prose, and punctuation and Unicode
the port could get wrong.

Usage:
  python3 checks/scripts/asks-corpus.py --check
      Portable check, no prototype needed: the fixture parses, its cases are the
      ones in this file, and each expected total is questions + statements.

  python3 checks/scripts/asks-corpus.py --check --reference PATH
      Also re-run the prototype on every text and compare what it says now.

  python3 checks/scripts/asks-corpus.py --write --reference PATH
      Regenerate the fixture from the prototype. Review the diff: a changed number
      is either a corpus mistake or a change in the specification, and the second
      has to be a stated decision.

  python3 checks/scripts/asks-corpus.py --reference PATH --actual OUT.json
      Differential run: compare the prototype with the Rust harness's `--json`
      output (`{"case-id": count, ...}`). The prototype is never a routine CI
      dependency; it is named here on purpose.

Exit codes: 0 as expected, 1 a difference, 2 cannot run (bad usage, missing file).
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import sys
from pathlib import Path

# The reference is read, never written: keep its folder free of .pyc files.
sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
FIXTURE = HERE.parent / "fixtures" / "asks-counts.json"
FORMAT = 1


def case(case_id: str, kind: str, note: str, text: str) -> dict:
    return {"id": case_id, "kind": kind, "note": note, "text": text}


# Kind is what the case is for, not what it counts: a `negative` can still hold a
# statement, and a `mixed` case has asks of more than one kind in it.
CASES = [
    # The 24 examples `questions.py` ships, verbatim, in its order.
    case("known-01-third-count-check", "known",
         "The example the prototype prints first: a direct offer with 'I'.",
         "Should I run a third count-only check?"),
    case("known-02-folder-sizes", "known",
         "A numbered list item with a modal and a person: 'May ... ?'.",
         "3. May T3 Agents measure folder sizes on disk? Today it reads only the database."),
    case("known-03-storage-label", "known",
         "A list item with a bold label: the rest is put to the reader.",
         "- **Storage:** do you want the warning in the bar too?"),
    case("known-04-why-parser", "known",
         "A self-question answered in the next sentence is not an ask.",
         "Why does this fail? The parser drops the quote."),
    case("known-05-why-test", "known",
         "A factual 'why' with nobody addressed and no answer still is not an ask.",
         "Why does the test fail?"),
    case("known-06-heading-question", "known",
         "A heading that is a question is a title, not an ask.",
         "## Why not the strictest form?"),
    case("known-07-decisions-heading", "known",
         "A scope heading puts the plain list under it in scope.",
         "## Decisions for you\n\n1. One table or six lenses?\n2. Ship the month first?"),
    case("known-08-fenced-code", "known",
         "A question inside a fenced block is code.",
         "```\nshould I do this?\n```"),
    case("known-09-block-quote", "known",
         "A question inside a block quote is quoted, not asked.",
         "> Should I keep it?"),
    case("known-10-inline-code", "known",
         "A question mark inside inline code does not end a sentence.",
         "Run `git log -- file?` to see"),
    case("known-11-answered-inline", "known",
         "A factual question answered in the same paragraph is the agent's own.",
         "Does the function work on a read-only connection? Let me test."),
    case("known-12-first-plugin", "known",
         "An agent's own 'did it work?' question.",
         "First, did the plugin load?"),
    case("known-13-fixtures-already", "known",
         "A lead-in that is not a scope word leaves the factual question unanswered.",
         "Before going further: do any tests in this checkout already read these fixtures? If one does, they could break."),
    case("known-14-want-me", "known",
         "An offer spelled as a question.",
         "Want me to start a Storage thread?"),
    case("known-15-build-green", "known",
         "A factual question with no reader in it is not an ask.",
         "Is the build green?"),
    case("known-16-trace-or-list", "known",
         "A choice offered to the reader in a list item.",
         "1. Trace first, or the task list first?"),
    case("known-17-video-quote", "known",
         "A question inside quotation marks is quoted, even mid-sentence.",
         "The video opens on \"Why not just put it in CLAUDE.md?\", plus a still if you'd rather post an image."),
    case("known-18-one-question-label", "known",
         "A scope label announces the question that follows it.",
         "**One question for you:** may the reader also take a command's exit code? It is a number."),
    case("known-19-two-decisions", "known",
         "A lead-in naming decisions puts the list under it in scope, labels and all.",
         "Two decisions are still open before I write v1.1:\n\n1. **Classifier:** turn it on as an opt-in? It reads labels.\n2. **Deleted threads:** keep their counts? I'd keep them."),
    case("known-20-still-waiting", "known",
         "A statement: an ask with no question mark.",
         "Still waiting on your word for restoring the original install."),
    case("known-21-ssh-pager", "known",
         "'still open' about a pager is not about a decision.",
         "Your SSH command is still open in that pager."),
    case("known-22-time-bar", "known",
         "'waiting on you' inside a measurement is not a statement ask.",
         "- **The time bar adds up too:** tools 62h, model 70h, waiting on you 7.0h."),
    case("known-23-hard-wrapped-ask", "known",
         "One question over three hard-wrapped lines is one question.",
         "May I run it once on your database,\nread-only and numbers only as before,\nto confirm the counting?"),
    case("known-24-parser-or-lexer", "known",
         "A choice phrased as a question, answered at once, is the agent's own.",
         "Is it the parser or the lexer? Checking both."),

    # Offers and decisions put to the reader.
    case("pos-may-i-push", "positive", "The shortest form of 'may I'.", "May I push this branch?"),
    case("pos-should-we-keep-flag", "positive", "'should we' is leave to act.", "Should we keep the old flag?"),
    case("pos-could-i-second-opinion", "positive", "'could I' asks for a person's judgement.",
         "Could I get a second opinion on the naming?"),
    case("pos-want-me-checks", "positive", "An offer of work.", "Want me to run the checks again?"),
    case("pos-is-it-ok-delete", "positive", "'is it OK if' asks permission.",
         "Is it OK if I delete the scratch folder?"),
    case("pos-any-preference", "positive", "An invitation to state a preference.",
         "Any preference between the two names?"),
    case("pos-sound-good", "positive", "'sound good' asks for agreement.", "Sound good to merge?"),
    case("pos-go-ahead", "positive", "'go ahead' asks leave to proceed.", "Go ahead with the deletion?"),
    case("pos-ship-or-hold", "positive", "A choice with 'or' and no factual opening.",
         "Ship it Friday, or hold until Monday?"),
    case("pos-do-we-or-wait", "positive", "A choice opened by 'do we', which is leave.",
         "Do we ship on Friday, or wait for the review?"),
    case("pos-your-call", "positive", "'your call' addresses the reader directly.",
         "It's your call: keep the warning in the bar?"),
    case("pos-do-you-approve", "positive", "A direct question with 'you'.", "Do you approve the smaller diff?"),
    case("pos-parens", "positive", "Parentheses around the question are edge punctuation.",
         "(Should I ship it?)"),
    case("pos-brackets", "positive", "Square brackets around the question are edge punctuation.",
         "[Should I ship it?]"),

    # Questions that are not put to the reader.
    case("neg-ship-it", "negative", "A bare 'Ship it?' names no reader and offers no choice.",
         "Ship it?"),
    case("neg-status-label", "negative", "A plain label does not put a factual question in scope.",
         "Status: is the build green?"),
    case("neg-bold-line", "negative", "A whole bold line is a heading, so its question is a title.",
         "**Should I ship it?**"),
    case("neg-table", "negative", "A table row is not prose.",
         "| Question | Owner |\n| --- | --- |\n| Should I ship it? | you |"),
    case("neg-indented-code", "negative", "Four spaces at the start of a line is a code block.",
         "    should I do this?\n    and this?"),
    case("neg-tilde-fence", "negative", "A tilde fence hides a question the same way backticks do.",
         "~~~\nMay I ship it?\n~~~"),
    case("neg-inline-code-label", "negative", "A question mark inside inline code is not punctuation.",
         "Use `should I ship it?` as the label."),
    case("neg-curly-quoted", "negative", "Curly quotation marks quote a question too.",
         "The note said \u201cShould I ship this?\u201d before the build."),
    case("neg-url", "negative", "A URL is a URL, even when its query holds a question mark.",
         "See https://example.test/should-i-go?x=1 for the detail."),
    case("neg-image", "negative", "Alt text is not prose.", "![Should I ship it?](shot.png)"),
    case("neg-which-parser", "negative", "A factual 'which' answered at once is the agent's own.",
         "Which parser is faster? The new one, by 12%."),
    case("neg-blockquote-multi", "negative", "A quoted block keeps its questions out of scope.",
         "> Should I keep it?\n> It is older."),
    case("neg-question-mark-only", "negative", "A question mark alone is shorter than a sentence.",
         "?"),
    case("neg-empty", "negative", "An empty message counts nothing.", ""),
    case("neg-whitespace", "negative", "Whitespace only counts nothing.", "  \n\t\n"),
    case("neg-fullwidth-question", "negative",
         "A full-width question mark is not the ASCII one the rule looks for.",
         "May I ship it\uff1f"),
    case("neg-bare-inline-code", "negative", "A question mark inside inline code alone is code.",
         "`May I ship it?`"),
    case("neg-plain-list", "negative", "Plain list items are not under a scope heading or lead-in.",
         "- Ship it Friday?\n- Keep the flag?"),
    case("neg-plan-lead-in", "negative", "A lead-in that names no question or decision leaves the list alone.",
         "Here is the plan:\n\n1. Ship it?"),
    case("neg-which-name", "negative", "A bare 'which' names no reader.", "Which name?"),

    # Statements: the same ask without a question mark.
    case("stmt-needs-ok", "statement", "A need stated as a fact.", "Needs your OK before I merge."),
    case("stmt-say-word", "statement", "An invitation to speak.", "Say the word and I'll add the flag."),
    case("stmt-list", "statement", "A statement in a list item still asks.", "- **Storage:** say the word and I'll add it."),
    case("stmt-awaiting", "statement", "'awaiting your' is a statement ask.",
         "Awaiting your review on the smaller diff."),
    case("stmt-let-me-know", "statement", "'let me know which' is a statement ask.",
         "Let me know which name you prefer."),
    case("stmt-still-open-with-you", "statement", "'still open with you' names a decision.",
         "The decision is still open with you."),
    case("stmt-hard-wrapped", "statement", "A statement over two hard-wrapped lines.",
         "Still waiting on your word\nfor the install."),
    case("stmt-after-question", "mixed",
         "A question in the paragraph means the statement is not counted again.",
         "May I merge? Still waiting on your word."),

    # Markdown structure.
    case("md-open-questions-heading", "markdown",
         "A scope heading puts a plain list item in scope.",
         "## Open questions\n\n- Ship it Friday?"),
    case("md-bold-decisions-heading", "markdown",
         "A whole bold line works as a heading, and its word puts the list in scope.",
         "**Decisions:**\n\n- Keep the flag?\n- Drop the flag?"),
    case("md-label-plain", "markdown", "A capitalised label before the sentence.",
         "Storage: do you want the warning in the bar?"),
    case("md-label-in-list", "markdown", "A bold label inside a list item.",
         "- **Naming:** do you want the shorter name instead?"),
    case("md-lead-in-list", "markdown", "A question lead-in puts the numbered list in scope.",
         "Two questions before I start:\n\n1. Which name?\n2. Keep the flag?"),
    case("md-heading-answered", "markdown", "A question heading over an ordinary paragraph.",
         "## What changed?\n\nThe reader now counts asks."),
    case("md-notes-then-ask", "markdown", "An ordinary heading does not stop the ask under it.",
         "## Notes\n\nShould I keep the long title?"),

    # Hard wrapping: one paragraph, many lines.
    case("wrap-question", "hard-wrap", "A question split across a line break.",
         "Do you want the warning in the bar,\nor only in the log?"),
    case("wrap-label-question", "hard-wrap", "A label and its question split across a list item's lines.",
         "1. **Naming:** do you want the shorter\n   name instead?"),
    case("wrap-sentence-boundary", "hard-wrap", "A sentence ends at a line break and the ask follows.",
         "The flag is off by default.\nDo you want it on?"),

    # Punctuation and Unicode the port has to keep.
    case("uni-curly-apostrophe", "unicode", "A curly apostrophe still ends the word 'you'.",
         "You\u2019re right \u2014 should we keep it?"),
    case("uni-ellipsis", "unicode", "An ellipsis before a direct question.",
         "So\u2026 do you want it?"),
    case("uni-emoji", "unicode", "An emoji inside a direct question.",
         "Should I ship it \U0001f680?"),
    case("uni-cjk", "unicode", "CJK text inside a direct question.",
         "Should I start with the \u65e5\u672c\u8a9e labels?"),
    case("uni-accent", "unicode", "An accented word next to the addressed reader.",
         "Do you want the caf\u00e9 name?"),
    case("uni-nbsp", "unicode", "A no-break space after a modal.",
         "May\u00a0I ship it?"),
    case("punct-em-dash", "punctuation",
         "A modal after an em dash is not at the start of the sentence, so the reference counts 0.",
         "It is done \u2014 should I tag the release?"),
    case("punct-question-exclaim", "punctuation", "A question and an exclamation is not the question mark the rule needs.",
         "Should I ship it?!"),
    case("punct-two-questions", "punctuation", "Two direct questions in one paragraph.",
         "May I merge? Want me to tag it?"),
    case("punct-mixed-sentences", "mixed", "A statement, then two asks.",
         "The build is green. Should I merge? Want me to tag it?"),

    # Mixed paragraphs, where a self-question sits beside a real ask.
    case("mix-self-then-ask", "mixed", "An answered self-question does not hide the ask after it.",
         "First, did the plugin load? It did. May I ship it?"),
    case("mix-why-then-ask", "mixed", "An answered 'why' beside a real offer.",
         "Why does it fail? Because the quote is dropped. Want me to fix it?"),
    case("link-text-question", "differential",
         "A link's text is prose, so the reference keeps its question (it strips only the target).",
         "[Should I ship it?](https://example.test/thread)"),
]


def load_reference(path: Path):
    if not path.is_file():
        fail(f"no reference at {path}")
    spec = importlib.util.spec_from_file_location("asks_reference", path)
    if spec is None or spec.loader is None:
        fail(f"cannot load the reference at {path}")
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except Exception as error:  # noqa: BLE001 - report what the reference did
        fail(f"the reference at {path} did not load: {error}")
    if not callable(getattr(module, "asks", None)):
        fail(f"the reference at {path} has no asks(text)")
    return module


def reference_counts(module, text: str) -> dict:
    """The prototype's answer for one text, as the corpus records it."""
    value = module.asks(text)
    if isinstance(value, bool):
        fail(f"asks() returned a bool for {text!r}")
    if isinstance(value, int):
        questions, statements, shape = value, 0, "int"
    elif isinstance(value, (tuple, list)) and len(value) == 2:
        questions, statements, shape = value[0], value[1], "pair(questions, statements)"
    else:
        fail(f"asks() returned {type(value).__name__} for {text!r}; expected an int or a pair")
    for name, number in (("questions", questions), ("statements", statements)):
        if isinstance(number, bool) or not isinstance(number, int) or number < 0:
            fail(f"asks() returned {name}={number!r} for {text!r}")
    return {"questions": questions, "statements": statements, "total": questions + statements,
            "shape": shape}


def expected(module) -> tuple[list, str]:
    out, shapes = [], set()
    for item in CASES:
        counts = reference_counts(module, item["text"])
        shapes.add(counts.pop("shape"))
        out.append({**item, "expected": counts})
    if len(shapes) != 1:
        fail(f"the reference answered in more than one shape: {sorted(shapes)}")
    return out, shapes.pop()


def render(shape: str, cases: list) -> str:
    header = [
        "{",
        f'  "format": {FORMAT},',
        '  "source": "the asks() prototype from the Insights thread (2026-11-18), on made-up texts only",',
        '  "counted": "questions + statements",',
        f'  "reference_shape": {json.dumps(shape)},',
        '  "cases": [',
    ]
    body = []
    for item in cases:
        body.append("    " + json.dumps(item, ensure_ascii=False,
                                        separators=(", ", ": ")))
    lines = header + [line + ("," if i < len(body) - 1 else "") for i, line in enumerate(body)]
    lines += ["  ]", "}", ""]
    return "\n".join(lines)


def load_fixture(path: Path) -> dict:
    if not path.is_file():
        fail(f"no corpus at {path}; run --write --reference PATH")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        fail(f"{path} is not JSON: {error}")


def check_fixture(path: Path, module=None) -> int:
    """Portable shape check, plus the prototype when one was named."""
    fixture = load_fixture(path)
    if fixture.get("format") != FORMAT:
        fail(f"{path}: format {fixture.get('format')!r}, expected {FORMAT}")
    cases = fixture.get("cases")
    if not isinstance(cases, list) or not cases:
        fail(f"{path}: no cases")
    if len(cases) != len(CASES):
        fail(f"{path}: {len(cases)} cases, this script has {len(CASES)}")
    seen = set()
    shape = None
    for got, want in zip(cases, CASES):
        if not isinstance(got, dict):
            fail(f"{path}: case {want['id']}: not an object")
        for key in ("id", "kind", "note", "text"):
            if got.get(key) != want[key]:
                fail(f"{path}: case {want['id']}: {key} differs from this script")
        if got["id"] in seen:
            fail(f"{path}: duplicate id {got['id']}")
        seen.add(got["id"])
        counts = got.get("expected") or {}
        for key in ("questions", "statements", "total"):
            number = counts.get(key)
            if isinstance(number, bool) or not isinstance(number, int) or number < 0:
                fail(f"{path}: {got['id']}: expected.{key} is not a count")
        if counts["total"] != counts["questions"] + counts["statements"]:
            fail(f"{path}: {got['id']}: total is not questions + statements")
        if module is not None:
            live = reference_counts(module, want["text"])
            case_shape = live.pop("shape")
            if shape is None:
                shape = case_shape
            elif shape != case_shape:
                fail(f"{path}: the reference answered in more than one shape")
            if live != counts:
                fail(f"{path}: {got['id']}: corpus says {counts}, the reference now says {live}")
    if module is not None and fixture.get("reference_shape") != shape:
        fail(f"{path}: reference_shape is {fixture.get('reference_shape')!r}, the reference is {shape!r}")
    print(f"asks corpus: {len(cases)} cases, all self-consistent"
          + (", and the reference agrees with every expected count" if module else ""))
    return 0


def write_fixture(path: Path, module) -> int:
    cases, shape = expected(module)
    path.write_text(render(shape, cases), encoding="utf-8")
    print(f"asks corpus: wrote {len(cases)} cases to {path}")
    return 0


def differential(module, actual_path: Path) -> int:
    try:
        actual = json.loads(actual_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        fail(f"no harness output at {actual_path}")
    except json.JSONDecodeError as error:
        fail(f"{actual_path} is not JSON: {error}")
    if not isinstance(actual, dict):
        fail(f"{actual_path}: expected an object of case id to count")
    differences = 0
    for item in CASES:
        want = reference_counts(module, item["text"])
        if item["id"] not in actual:
            print(f"missing  {item['id']}: the harness did not report a count")
            differences += 1
            continue
        got = actual[item["id"]]
        if isinstance(got, bool) or not isinstance(got, int):
            print(f"bad      {item['id']}: the harness reported {got!r}")
            differences += 1
        elif got != want["total"]:
            print(f"differs  {item['id']}: reference {want['total']}, product {got}")
            differences += 1
    extra = set(actual) - {item["id"] for item in CASES}
    for case_id in sorted(extra):
        print(f"extra    {case_id}: the harness reported a case the corpus does not have")
        differences += 1
    if differences:
        print(f"asks differential: {differences} difference(s) of {len(CASES)} cases")
        return 1
    print(f"asks differential: {len(CASES)} of {len(CASES)} cases agree")
    return 0


def fail(message: str):
    print(f"asks corpus: {message}", file=sys.stderr)
    raise SystemExit(2)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="The asks-count corpus and its reference run.")
    parser.add_argument("--reference", type=Path,
                        help="asks/questions.py to use; omit for the portable check")
    parser.add_argument("--fixture", type=Path, default=FIXTURE,
                        help=f"corpus to read or write (default {FIXTURE})")
    parser.add_argument("--write", action="store_true",
                        help="regenerate the corpus from --reference")
    parser.add_argument("--check", action="store_true",
                        help="hold the corpus to this script, and to the reference if named")
    parser.add_argument("--actual", type=Path,
                        help="the Rust harness's --json output, for a differential run")
    args = parser.parse_args(argv)

    if args.write:
        if not args.reference:
            fail("--write needs --reference PATH")
        return write_fixture(args.fixture, load_reference(args.reference))
    if args.actual:
        if not args.reference:
            fail("--actual needs --reference PATH, so both sides are named")
        return differential(load_reference(args.reference), args.actual)
    # --check is the default, so `asks-corpus.py` never silently rewrites anything.
    module = load_reference(args.reference) if args.reference else None
    return check_fixture(args.fixture, module)


if __name__ == "__main__":
    sys.exit(main())
