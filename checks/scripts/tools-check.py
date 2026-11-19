#!/usr/bin/env python3
"""An independent check of `t3-agents --tools`: the numbers, counted a second time.

python3 checks/scripts/tools-check.py [--seed N] [--cases N] [--only NAME] [--keep]

It builds seeded random T3 databases from the checked-in schema fixtures, works out the
expected `Tools` value from the generator's own in-memory records (never by querying the
database it wrote, except to prove the file did not change), runs the built program on
them, and compares every field of its JSON exactly.

The reference below is written from the report's written rules (what counts as a call,
how time is added up, how the rows are worked out), not from `crates/core/src/tools.rs`.
Two pieces of
`recap.rs` it has to agree with are copied here as procedure and named: how an ISO
timestamp is read (`ms`) and how a period's local midnights are found (`Period::new`).
A call belongs to a period by its instant, not by its spelling: it counts when `ms` reads its
`startedAt` at or after the period's first instant and before its end, compared as numbers,
so every spelling of one instant is in or out together.
Command lines are a fixture, not a parser: `LINES` is the table in `tools.rs`, copied
with its programs and labels, and this script only looks lines up in it.

Needs `cargo build --release --locked` first. Standard library only. Every database it
reads is one it built under checks/generated/tools-check/, removed when it ends.
The program inherits T3_AGENTS_READ_THREADS, T3_AGENTS_SKIM, T3_AGENTS_CACHE,
T3_AGENTS_NO_CACHE, and T3_AGENTS_CACHE_STATS so the reader paths can be checked.
Cache checks use a scratch XDG_CACHE_HOME and compare raw output before and after a write.
"""
import argparse, collections, hashlib, json, math, os, random, re, shutil, sqlite3, subprocess, sys, time
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[2]
BINARY = Path(os.environ.get('T3_AGENTS_BINARY') or ROOT / 'target/release/t3-agents-term')
P = 'orchestration_v2_projection_'
HOUR, DAY = 3_600_000, 86_400_000
SENTINEL = 'never read'
# The report has one sentence of its own with the planted word in it ("3 calls never ended
# and add no time"). Outside that phrase, and anywhere in --json, the word is a leak.
OWN_WORDS = ('never ended',)


def leaked(text, prose):
    """Whether the planted word is in what the program wrote, beyond the report's own words."""
    text = text.lower()
    if prose:
        for phrase in OWN_WORDS:
            text = text.replace(phrase, '')
    return 'never' in text
DERIVE = object()       # a field the generator works out for itself
ABSENT = object()       # a JSON key that is not written at all

# Command lines the generator writes, each with the programs the shared parser
# (crates/core/src/program.rs) lists for it, in order, and its label. Copied as data from
# the `LINES` table in crates/core/src/tools.rs, whose own test holds the parser to every
# row. This script has no parser, no list of programs and no labelling rule.
LINES = [
    ("sed -n '1,4p' notes.txt", ('sed',), 'sed'),
    ("cd app && sed -n '1,4p' notes.txt | tail -20", ('sed', 'tail'), 'sed'),
    ('bash -lc "sed -n \'1,4p\' notes.txt"', ('sed',), 'sed'),
    ('/usr/bin/git log', ('git',), 'git'),
    ('cd app && git diff | tail -20', ('git', 'tail'), 'git'),
    ('bash -lc "git status"', ('git',), 'git'),
    ('cat notes.txt', ('cat',), 'cat'),
    ('cd app && cat notes.txt | tail -20', ('cat', 'tail'), 'cat'),
    ('bash -lc "cat notes.txt"', ('cat',), 'cat'),
    ('grep test notes.txt', ('grep',), 'grep'),
    ('cd app && grep test notes.txt | tail -20', ('grep', 'tail'), 'grep'),
    ('bash -lc "grep test notes.txt"', ('grep',), 'grep'),
    ('rg -n foo | head', ('rg', 'head'), 'rg'),
    ('cd app && rg test | tail -20', ('rg', 'tail'), 'rg'),
    ('bash -lc "rg test"', ('rg',), 'rg'),
    ('cargo test -p x', ('cargo',), 'cargo'),
    ('cd app && cargo test | tail -20', ('cargo', 'tail'), 'cargo'),
    ('bash -lc "cd /x && cargo test 2>&1 | tail -20"', ('cargo', 'tail'), 'cargo'),
    ('python3 run.py', ('python3',), 'python3'),
    ('cd app && python3 run.py | tail -20', ('python3', 'tail'), 'python3'),
    ('bash -lc "python3 run.py"', ('python3',), 'python3'),
    ('./run-checks.sh', (), None),
    ('bash scripts/build.sh', (), None),
    ('omabox run demo', (), None),
    ('bash -lc "./run-checks.sh"', (), None),
    ('cd x && FOO=1 python3 run.py', ('python3',), 'python3'),
    ('mkdir -p out && cargo build --release', ('mkdir', 'cargo'), 'cargo'),
    ('export A=1; npm test', ('npm',), 'npm'),
    ('echo "a && cargo test"', ('echo',), 'echo'),
    ("echo 'x | rg y'", ('echo',), 'echo'),
    ('cat <<EOF > f.sh\ncargo test\nEOF\ngit status', ('cat', 'git'), 'git'),
    ('x=$(git rev-parse HEAD); echo $x', ('echo',), 'echo'),
    ('echo `cargo --version`', ('echo',), 'echo'),
    ('for f in *.rs; do rustc $f; done', ('rustc',), 'rustc'),
    ('if cargo check; then git commit; fi', ('cargo', 'git'), 'cargo'),
    ('sudo -E env A=1 timeout 60 make test', ('make',), 'make'),
    ('(cd x && go test ./...) && git diff', ('go', 'git'), 'go'),
    ('sh -c \'bash -c "sh -c cargo"\'', (), None),
    ('bash build.sh', (), None),
    ('./never-read --flag', (), None),
    ('never-read-program arg', (), None),
    ('# cargo test', (), None),
    ('', (), None),
    ('cargo test -p x # never read', ('cargo',), 'cargo'),
]
LOOKUP = {line: (programs, label) for line, programs, label in LINES}
DISTINCT_PROGRAMS = len({p for _, programs, _ in LINES for p in programs})

TOOL_TYPES = ('command_execution', 'dynamic_tool', 'file_change', 'file_search', 'web_search')
IGNORED_TYPES = ('reasoning', 'assistant_message', 'user_message', 'subagent', 'checkpoint', 'error', 'compaction')
BUCKETS = (('under 1s', 1_000), ('1-10s', 10_000), ('10-60s', 60_000), ('1-10min', 600_000), ('over 10min', None))
READ_NAMES = ('read', 'view', 'view_image', 'notebookread')
SEARCH_NAMES = ('glob', 'grep', 'ls')
IDENT = set('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.:/-')
WEEKDAYS = ('Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun')
STATUSES = ('completed', 'failed', 'interrupted', 'cancelled', 'running')
PROVIDERS = ('claudeAgent', 'codex', 'grok', 'opencode', 'Codex', 'ClaudeAgent', 'GrokBeta', None, '')
OPTIONS = (('none', (), 'last-week'), ('--this-week', ('--this-week',), 'this-week'),
           ('--month', ('--month',), 'last-month'), ('--this-month', ('--this-month',), 'this-month'))

TEXT_FIELDS = ('output', 'text', 'title', 'fileName', 'pattern', 'patterns', 'prompt', 'message',
               'summary', 'result', 'results', 'questions', 'changes')

# What the expected values must actually have exercised. A run that never reaches one of
# these has not checked what it claims to, so it fails.
REQUIRED_COVER = (
    'a bound spelled so its text sorts before it: in or out by its instant',
    'longest calls tied on run ordinal and ordered by rowid',
    'call at exactly the first instant, counted', 'call at exactly the end, left out',
    'call 1 ms before the first instant, left out', 'call 1 ms before the end, counted',
    'call that ends after the period, counted whole',
    'open call: end absent', 'open call: end null', 'open call: end a number',
    'open call: end not a timestamp', 'open call: end before the start', 'call of zero length',
    'run closed exactly at the first instant', 'run requested exactly at the end',
    'call in the period whose run is not open, left out',
    'no-run call found by its own dates only', 'no-run call found by a subagents row only',
    'no-run call found by both routes', 'no-run call on a thread found by neither route, left out',
    'closed together: starts 999 ms apart, left alone', 'closed together: starts 1000 ms apart, left alone',
    'closed together: starts 1001 ms apart', 'closed together: starts minutes apart',
    'closed together: a parallel batch with one start', 'closed together: the same end in two threads',
    'closed together: a single call at that end, left alone',
    'exit code -1', 'exit code 0', 'exit code 1', 'exit code 2', 'exit code 3', 'exit code 124',
    'exit code 125', 'exit code 126', 'exit code 127', 'exit code 128', 'exit code 129', 'exit code 255',
    'exit code 256', 'exit code 2**40', 'exit code not an integer', 'exit code on a non-command',
    'command marked failed with an exit code', 'command not marked failed with an exit code',
    'tool name of exactly 80 characters', 'tool name of 81 characters', 'tool name empty',
    'tool name a number', 'tool name not an identifier', 'tool name in mcp__S__T',
    'tool name in server.name', 'tool name in mixed case', 'tool name not on the receiver',
    'a provider not recorded', 'four or more providers',
    'every status', 'every tool type', 'an item of a type that must be ignored',
    'chain labelled by its weightiest program', 'program that appears in a chain but labels none',
    'command with no listed program', 'overlapping calls in one thread', 'overlapping calls in two threads',
    'days before the first record', 'the last period empty, so this one is shown',
    'a period with no calls', 'a sentinel in every text field', 'a command line the list does not know',
    'a run whose id no row has', 'a call attributed to its run\'s thread, not its own',
)
EXIT_TAGS = {-1: 'exit code -1', 0: 'exit code 0', 1: 'exit code 1', 2: 'exit code 2', 3: 'exit code 3',
             124: 'exit code 124', 125: 'exit code 125', 126: 'exit code 126', 127: 'exit code 127',
             128: 'exit code 128', 129: 'exit code 129', 255: 'exit code 255', 256: 'exit code 256',
             2 ** 40: 'exit code 2**40'}
EXIT_CODES = (-1, 0, 1, 2, 3, 124, 125, 126, 127, 128, 129, 130, 137, 143, 254, 255, 256, 1000, -255, 2 ** 40)
EXIT_NOT_INTEGER = ('1', '0', '', 1.5, 1.0, 0.0, None, True, False, [1], {'code': 1})
LEN_EDGES = (0, 1, 999, 1000, 1001, 9_999, 10_000, 59_999, 60_000, 599_999, 600_000, 600_001)


class Cover(collections.Counter):
    def require(self, keys):
        missing = [k for k in keys if not self[k]]
        if missing:
            raise SystemExit('the reference never exercised: ' + ', '.join(missing))


COVER = Cover()
STATUS_SEEN, TYPE_SEEN, KIND_SEEN = set(), set(), set()


# ── time, by our own code ──────────────────────────────────────────────────────

def days_from_civil(y, m, d):
    y = y - 1 if m <= 2 else y
    era = y // 400
    yoe = y - era * 400
    doy = (153 * (m - 3 if m > 2 else m + 9) + 2) // 5 + d - 1
    return era * 146097 + yoe * 365 + yoe // 4 - yoe // 100 + doy - 719468


def _i64(raw):
    """The integer Rust's `str::parse::<i64>` accepts: an optional sign, then digits."""
    try:
        text = raw.decode('ascii')
    except UnicodeDecodeError:
        return None
    body = text[1:] if text[:1] in '+-' else text
    if not body or any(c not in '0123456789' for c in body):
        return None
    value = int(text)
    return value if -2 ** 63 <= value < 2 ** 63 else None


def ms(value):
    """Epoch milliseconds of a T3 timestamp, or None. Copies `recap::ms`, byte for byte:
    `2026-11-11T22:15:46.654Z` to epoch ms; some other text parses too."""
    if not isinstance(value, str):
        return None
    b = value.encode('utf-8')
    if len(b) < 19 or b[4:5] != b'-' or b[7:8] != b'-' or b[10:11] != b'T':
        return None
    parts = [_i64(b[a:z]) for a, z in ((0, 4), (5, 7), (8, 10), (11, 13), (14, 16), (17, 19))]
    if any(part is None for part in parts):
        return None
    y, mo, d, h, mi, s = parts
    secs = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s
    frac = 0.0
    if len(b) > 19 and b[19:20] == b'.':
        digits = ''
        for c in b[20:]:
            if 48 <= c <= 57:
                digits += chr(c)
            else:
                break
        frac = float('0.' + digits) if digits else 0.0
    return secs * 1000.0 + math.floor(frac * 1000.0 + 0.5)


def iso(stamp):
    """Epoch milliseconds to the form T3 writes. Copies `recap::iso`."""
    millis = math.floor(stamp)
    secs, milli = divmod(millis, 1000)
    days, rem = divmod(secs, 86400)
    z = days + 719468
    era = z // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp = (5 * doy + 2) // 153
    d = doy - (153 * mp + 2) // 5 + 1
    m = mp + 3 if mp < 10 else mp - 9
    y = yoe + era * 400 + (1 if m <= 2 else 0)
    return '%04d-%02d-%02dT%02d:%02d:%02d.%03dZ' % (y, m, d, rem // 3600, rem % 3600 // 60, rem % 60, milli)


class Zone:
    def __init__(self, name):
        self.name, self.tz = name, ZoneInfo(name)

    def at(self, seconds):
        d = datetime.fromtimestamp(seconds, self.tz)
        return (d.year, d.month, d.day, d.hour, d.minute, d.second)

    def midnight(self, seconds):
        """The local midnight at or before `seconds`, with a moved hour put back. Copies
        `recap::midnight`."""
        y, mo, d, h, mi, s = self.at(seconds)
        start = seconds - (h * 3600 + mi * 60 + s)
        hour = self.at(start)[3]
        return start + 3600 if hour == 23 else start - 3600 if hour == 1 else start

    def weekday(self, seconds):
        y, mo, d = self.at(seconds)[:3]
        return (days_from_civil(y, mo, d) + 3) % 7

    def date(self, stamp):
        y, mo, d = self.at(math.floor(stamp / 1000))[:3]
        return '%04d-%02d-%02d' % (y, mo, d)

    def when(self, stamp, span):
        y, mo, d, h, mi, _ = self.at(math.floor(stamp / 1000))
        day = WEEKDAYS[(days_from_civil(y, mo, d) + 3) % 7]
        return '%s %02d:%02d' % (day, h, mi) if span == 'week' else '%s %d, %02d:%02d' % (day, d, h, mi)


def period(now, which, zone):
    """The local midnights of a week (Monday to Sunday) or a calendar month. Copies
    `recap::Period::new`, including its 26-hour step and the `until` clamp."""
    first = zone.midnight(math.floor(now / 1000))
    if which in ('LastWeek', 'ThisWeek'):
        for _ in range(zone.weekday(first) + (7 if which == 'LastWeek' else 0)):
            first = zone.midnight(first - 7200)
        length = 7
    else:
        def to_first(day):
            while zone.at(day)[2] > 1:
                day = zone.midnight(day - 7200)
            return day
        first = to_first(first)
        if which == 'LastMonth':
            first = to_first(zone.midnight(first - 7200))
        y, mo = zone.at(first)[:2]
        ny, nm = (y + 1, 1) if mo == 12 else (y, mo + 1)
        length = days_from_civil(ny, nm, 1) - days_from_civil(y, mo, 1)
    days, day = [], first
    for _ in range(length + 1):
        days.append(day * 1000)
        day = zone.midnight(day + 26 * 3600)
    end = days[-1]
    name = {'LastWeek': 'last-week', 'ThisWeek': 'this-week', 'LastMonth': 'last-month', 'ThisMonth': 'this-month'}[which]
    return dict(which=which, name=name, days=days, start=days[0], end=end, last_day=days[-2], now=now,
                until=min(end, max(now, days[0])), span='month' if 'Month' in which else 'week',
                so_far=which.startswith('This'), n=length)


# ── the world: what the generator wrote, as plain records ──────────────────────

class World:
    def __init__(self, variant, zone, now, mode):
        self.variant, self.zone, self.now, self.mode = variant, zone, now, mode
        self.threads, self.runs, self.items, self.subs = [], [], [], []
        p = {w: period(now, w, zone) for w in ('LastWeek', 'ThisWeek', 'LastMonth', 'ThisMonth')}
        self.periods = {v['name']: v for v in p.values()}
        self.has_sub_columns = variant != 'no-subagent-columns'
        self.n = 0
        self.ordinal = 0
        self.tie_runs = set()
        self.starts = set()

    def nid(self, prefix):
        self.n += 1
        return '%s-%d' % (prefix, self.n)

    def uniq(self, start):
        while start in self.starts:
            start += 1
        self.starts.add(start)
        return start


def clean(d):
    return {k: v for k, v in d.items() if v is not ABSENT}


def add_thread(w, provider, created, updated, rel=None):
    t = dict(id=w.nid('thread'), provider=provider, created=created, updated=updated, rel=rel)
    w.threads.append(t)
    return t


def add_run(w, rng, thread, requested, completed, status=None, provider=DERIVE):
    run = dict(id=w.nid('run'), thread=thread['id'],
               ordinal=sum(1 for r in w.runs if r['thread'] == thread['id']) + 1,
               requested=iso(requested), completed=None if completed is None else iso(completed),
               provider=thread['provider'] if provider is DERIVE else provider,
               status=status or rng.choice(('completed', 'failed', 'interrupted', 'running', 'cancelled')))
    w.runs.append(run)
    return run


NAME_POOL = [
    'Read', 'read', 'READ', 'View', 'view_image', 'NotebookRead', 'notebookread', 'Glob', 'Grep', 'ls', 'LS',
    'ToolSearch', 'TaskStop', 'TodoWrite', 'WebFetch', 'readfile', 'Read ',
    'mcp__t3-code__delegate_task', 'mcp__t3-code__delegate_task', 'mcp__github__get_issue', 'mcp__srv__read',
    'mcp__srv__Grep', 'mcp__a__b__c', 'mcp__t3_code__x', 'Mcp__x__y', 'MCP__X__Y', 'mcp__a___b',
    't3-code.delegate_task', 't3-code.delegate_task', 'codex_apps.github.fetch', 'codex_apps.github.fetch',
    'a.b.c.d', 'read.x', 'x.read', '.x', 'x.', '.', 'a:b/c-d.e', 'srv.ls', 'mcp__a.b', 'mcp__s.s__t',
    'mcp____x', 'mcp__a__', 'mcp__a', 'mcp__', 'mcp____', '', 'n' * 80, 'n' * 80 + 'x',
    'mcp__srv__' + 'n' * 72, 'mcp__srv__' + 'n' * 73, 'a.' + 'n' * 78, 'a.' + 'n' * 79,
    'run this: "never read"', 'never read', 'a b', 'x"y', "x'y", '\u00e9', 'tool\nx', 'a*b', '$x', 'a;b',
    'a,b', 'a\\b', 'mcp__never read__x', 'never read.x', ABSENT, None, 12345, 0, 3.5, True,
]


def rand_len(rng):
    r = rng.random()
    if r < 0.05: return rng.choice(LEN_EDGES)
    if r < 0.62: return rng.randrange(1, 1000)
    if r < 0.82: return rng.randrange(1000, 10_000)
    if r < 0.92: return rng.randrange(10_000, 60_000)
    if r < 0.97: return rng.randrange(60_000, 600_000)
    return rng.randrange(600_000, 3 * HOUR)


def add_item(w, rng, thread, run, type='command_execution', status=None, start=DERIVE, length=DERIVE,
             started=DERIVE, completed=DERIVE, command=DERIVE, tool=DERIVE, exit=DERIVE, unique=True):
    """One turn item, as T3 would record it. `start`/`length` are ms; `started`/`completed`
    override the text written, so a bad timestamp can be deliberate."""
    if status is None:
        status = rng.choices(STATUSES, (60, 14, 7, 6, 13))[0]
    if start is DERIVE:
        start = rng.randrange(w.lo, w.hi) if w.lo is not None else 0
    if unique:
        start = w.uniq(start)
    if length is DERIVE:
        length = rand_len(rng)
    if started is DERIVE:
        started = iso(start) if start % 1000 else iso(start)[:-4] + 'Z'
    if completed is DERIVE:
        completed = None if length is None else iso(start + length)
    if command is DERIVE:
        if type == 'command_execution':
            r = rng.random()
            command = rng.choice(LINES)[0] if r < 0.9 else rng.choice((ABSENT, None, 42, ''))
        else:
            command = ABSENT
    if tool is DERIVE:
        tool = rng.choice(NAME_POOL) if type == 'dynamic_tool' else ABSENT
    if exit is DERIVE:
        if type == 'command_execution':
            r = rng.random()
            exit = ABSENT if r < 0.3 else rng.choice(EXIT_NOT_INTEGER) if r < 0.42 else rng.choice(EXIT_CODES)
        else:
            exit = ABSENT
    payload = dict(type=rng.choice(STATUSES + ('compaction',)), status=rng.choice(STATUSES),
                   startedAt=started, completedAt=completed)
    for field in TEXT_FIELDS:
        payload[field] = [SENTINEL] if field in ('patterns', 'questions') else \
            [{'x': SENTINEL}] if field in ('results', 'changes') else '%s %s' % (SENTINEL, field)
    if command is not ABSENT: payload['input'] = command
    if tool is not ABSENT: payload['toolName'] = tool
    if exit is not ABSENT: payload['exitCode'] = exit
    if type != 'command_execution' and rng.random() < 0.5:   # a code on a non-command: never counted
        payload['exitCode'] = rng.choice(EXIT_CODES + EXIT_NOT_INTEGER)
    w.ordinal += 1
    item = dict(id=w.nid('item'), thread=thread['id'], run=run['id'] if run else None,
                ordinal=w.ordinal, type=type, status=status, payload=payload)
    w.items.append(item)
    return item


def build_world(rng, variant, tz, now, mode='wide'):
    w = World(variant, Zone(tz), now, mode)
    P = w.periods
    last, first_period = P['last-week'], P['last-week']
    if mode == 'this-only':
        w.lo, w.hi = P['this-week']['start'], now
    elif mode == 'last-only':
        w.lo, w.hi = last['start'], last['end'] - 1
    elif mode == 'empty':
        w.lo = w.hi = None
    else:
        w.lo = min(p['start'] for p in P.values()) - 2 * DAY
        w.hi = now + 2 * DAY
    if w.lo is not None and w.hi <= w.lo:
        w.lo = w.hi - HOUR
    # In a world built to leave the last period empty, the feature blocks must stay inside
    # the period that is meant to have calls, or the thing being tested is gone.
    window = P['this-week'] if mode == 'this-only' else P['last-week']
    blocks = [window] if mode in ('this-only', 'last-only') else list(P.values())
    # When the records begin: before every period, or inside one so some days come first.
    if w.lo is None:
        first = last['start'] - 30 * DAY
    elif rng.random() < 0.5:
        first = w.lo - 30 * DAY
    else:
        first = rng.randrange(max(w.lo, first_period['start']), w.hi)

    def prov():
        return rng.choice(PROVIDERS)

    oldest = add_thread(w, prov(), iso(first), iso(now if w.hi is None else w.hi), None)
    mains = [add_thread(w, p, iso(first + k * DAY), iso((w.hi or now) - k), None)
             for k, p in enumerate(('claudeAgent', 'codex', 'grok', 'opencode', 'Codex'))]
    sub_dates = add_thread(w, 'claudeAgent', iso(first), iso((w.hi or now) + HOUR), 'subagent')
    sub_dates_edge = add_thread(w, 'grok', iso(first), iso(now), 'subagent')
    sub_rows = add_thread(w, 'grok', iso(first), iso(now if w.hi is None else w.hi), rng.choice((None, 'root', 'fork')))
    sub_both = add_thread(w, 'opencode', iso(first), iso((w.hi or now) + HOUR), 'subagent')
    plain = add_thread(w, None, iso(first), iso(now), None)             # provider not recorded
    unrecorded = add_thread(w, ABSENT, iso(first), iso(now), None)      # provider key absent entirely
    orphan = 'thread-orphan'                                            # a subagents row with no thread
    w.n += 1

    # A child thread's own dates against one period's edges, sometimes found and sometimes not.
    p = rng.choice(list(P.values()))
    c, u = rng.choice(((p['start'] + HOUR, p['end'] - HOUR), (p['until'], p['until'] + HOUR),
                       (p['until'] - 1, p['until'] + HOUR), (p['start'] - DAY, p['start']),
                       (p['start'] - DAY, p['start'] - 1), (p['until'] + HOUR, p['until'] + 2 * HOUR),
                       (p['start'] - 2 * DAY, p['start'] - DAY)))
    sub_dates_edge['created'], sub_dates_edge['updated'] = iso(c), iso(u)
    if rng.random() < 0.5:
        sub_dates_edge['updated'] = rng.choice(('', 'soon', ABSENT))

    def sub_row(child, window=None):
        if window is None:
            p = rng.choice(list(P.values()))
            window = rng.choice(((p['start'] + HOUR, p['start'] + 2 * HOUR), (p['start'] - DAY, p['start']),
                                 (p['start'] - DAY, p['start'] - 1), (p['until'], p['until'] + HOUR),
                                 (p['until'] - 1, p['until'] + HOUR), (p['start'] - DAY, None),
                                 (None, p['start'] + HOUR), (p['start'] - 3 * DAY, p['end'] + DAY),
                                 (p['until'] + HOUR, None), (None, None)))
        s, c = window
        w.subs.append(dict(id=w.nid('sub'), thread=rng.choice(mains)['id'], child=child,
                           started=ABSENT if s is None else iso(s),
                           completed=ABSENT if c is None else iso(c)))
    for period_ in P.values():          # one route each, inside every period, so both are covered
        sub_row(sub_rows['id'], (period_['start'] + HOUR, period_['end'] - HOUR))
        sub_row(sub_both['id'], (period_['start'] + HOUR, period_['end'] - HOUR))
    sub_row(orphan)
    sub_row(None)
    sub_row(plain['id'])
    for _ in range(3):
        sub_row(rng.choice((sub_rows['id'], sub_dates_edge['id'], sub_both['id'])))

    # Runs: wide ones open across everything, then ordinary runs per thread.
    span_all = (w.lo - 45 * DAY) if w.lo is not None else first - 45 * DAY
    wide = [add_run(w, rng, t, span_all, None, 'running') for t in (mains[0], sub_dates, sub_both)]
    runs = []
    for t in mains + [sub_dates, sub_dates_edge, sub_rows, sub_both, plain, unrecorded, oldest]:
        for _ in range(rng.randrange(1, 4)):
            a = rng.randrange(w.lo, w.hi) if w.lo is not None else first + rng.randrange(0, DAY)
            runs.append(add_run(w, rng, t, a, None if rng.random() < 0.2 else a + rng.randrange(0, 8 * HOUR)))

    if w.lo is not None:
        runs_of = collections.defaultdict(list)
        for r in runs:
            runs_of[r['thread']].append(r)
        for t in mains + [sub_dates, sub_dates_edge, sub_both]:
            for r in runs_of[t['id']]:
                a = ms(r['requested'])
                span = (ms(r['completed']) - a) if r['completed'] else 8 * HOUR
                for _ in range(rng.choice((0, 1, 2, 5, 12, 25))):
                    add_item(w, rng, t, r, type=rng.choices(TOOL_TYPES + IGNORED_TYPES,
                                                            (50, 18, 6, 3, 3, 3, 3, 3, 3, 3, 2, 2))[0],
                             start=max(w.lo, int(a + rng.randrange(0, max(1, int(max(span, 1)) + 1)))))
        for t, r in zip((mains[0], sub_dates, sub_both), wide):
            for _ in range(14):
                add_item(w, rng, t, r, type=rng.choices(TOOL_TYPES, (55, 15, 10, 10, 10))[0])
            s = rng.randrange(window['start'], window['end'] - 2 * HOUR)
            add_item(w, rng, t, r, type='command_execution', start=s, length=1_500, status='completed')
            add_item(w, rng, t, r, type='command_execution', start=s + 500, length=1_500, status='completed')
        # The instants at every period's edges, in the wide run and with no run at all.
        for p in blocks:
            for instant, delta in ((p['start'], 0), (p['start'], -1), (p['until'], -1), (p['until'], 0)):
                # T3's text at a bound, with milliseconds and in whole seconds (the second a later
                # instant than the first when it drops them). And at the bound itself the same
                # instant spelled one second earlier with a fraction `ms` rounds up to it: it sorts
                # before the bound as text but is the bound, so it is in or out with it.
                spellings = [iso(instant + delta), iso(instant + delta)[:-4] + 'Z']
                if delta == 0:
                    spellings.append(iso(instant - 1000)[:-4] + '9996Z')
                for started in spellings:
                    add_item(w, rng, mains[0], wide[0], start=instant + delta, started=started,
                             completed=iso(instant + delta + HOUR), unique=False)
            add_item(w, rng, mains[0], wide[0], start=p['until'] - HOUR, length=3 * HOUR, unique=False)
            add_item(w, rng, sub_dates, None, start=p['start'], unique=False)
            add_item(w, rng, sub_dates, None, start=p['until'] - 1, unique=False)
            # Calls with no usable end, in a run and with no run: every shape a bad end has.
            at = p['start'] + 6 * HOUR
            for completed in (ABSENT, None, 12345, 'soon', iso(at - HOUR)):
                add_item(w, rng, mains[0], wide[0], start=at, completed=completed, unique=False)
            add_item(w, rng, mains[0], wide[0], start=at, completed=iso(at), unique=False)   # zero length
            add_item(w, rng, sub_dates, None, start=at, completed=ABSENT, unique=False)
        # A run that closed exactly at a period's first instant, and one asked for at its end.
        for p in blocks:
            t = rng.choice(mains)
            r = add_run(w, rng, t, p['start'] - 2 * HOUR, p['start'])
            add_item(w, rng, t, r, start=p['start'] + HOUR, unique=False)
            r = add_run(w, rng, t, p['start'] - 2 * HOUR, p['start'] - 1)
            add_item(w, rng, t, r, start=p['start'] + HOUR, unique=False)
            r = add_run(w, rng, t, p['until'], None)
            add_item(w, rng, t, r, start=p['until'] - HOUR, unique=False)

        # Runs open across everything, so the blocks below are found in every period.
        feature = {t['id']: add_run(w, rng, t, w.lo - 45 * DAY, None) for t in (mains[1], mains[2], mains[3], mains[4])}
        # A call whose run belongs to another thread: it belongs to the run's thread.
        r = wide[0]
        other = mains[3]
        add_item(w, rng, other, r, start=window['start'] + 30 * HOUR)
        add_item(w, rng, mains[4], feature[mains[1]['id']], start=window['start'] + 40 * HOUR)

        # A run on another thread, for a call that overlaps one in the first thread.
        r_cross = add_run(w, rng, mains[1], window['start'] - DAY, window['end'] + DAY)
        overlap = window['start'] + 5 * DAY
        add_item(w, rng, mains[0], wide[0], start=overlap, length=600_000, unique=False)
        add_item(w, rng, mains[1], r_cross, start=overlap, length=600_001, unique=False)

        # Calls closed together. Each group has its own thread and its own end, so nothing
        # else joins it. The listed spans are the whole point of the rule.
        def group(thread, run, end, spans, statuses=None):
            for k, span in enumerate(spans):
                add_item(w, rng, thread, run, status=(statuses or ['completed'])[k % len(statuses or ['completed'])],
                         start=end - span, completed=iso(end), unique=False)
        base = window['start'] + 2 * DAY
        group(mains[0], wide[0], base, (999, 0))
        group(mains[0], wide[0], base + HOUR, (1000, 0))
        group(mains[0], wide[0], base + 2 * HOUR, (1001, 0))
        group(mains[0], wide[0], base + 3 * HOUR, (9 * 60_000, 6 * 60_000, 60_000, 0), ('failed', 'completed'))
        add_item(w, rng, mains[0], wide[0], start=base + 4 * HOUR, completed=iso(base + 4 * HOUR + 500), unique=False)
        add_item(w, rng, mains[0], wide[0], start=base + 4 * HOUR, completed=iso(base + 4 * HOUR + 500), unique=False)
        group(mains[1], r_cross, base + 5 * HOUR, (0,))                 # a lone call at that end
        group(sub_dates, wide[1], base + 6 * HOUR, (0,))
        group(sub_both, wide[2], base + 6 * HOUR, (0,))                  # the same end, another thread

        if window['start'] + 3 < window['until']:
            tie_at = window['start'] + 1
            tie = add_run(w, rng, mains[0], tie_at, tie_at + 7 * DAY, status='completed')
            w.tie_runs.add(tie['id'])
            for n, pair in enumerate(((('input', '/usr/bin/git log'), ('tool', 'Read')),
                                      (('tool', 'Read'), ('tool', 'mcp__server__Lookup')),
                                      (('input', '/usr/bin/git log'), ('input', 'cargo test -p x')))):
                if rng.randrange(2): pair = pair[::-1]
                tied = [add_item(w, rng, mains[0], tie, start=tie_at + n, length=7 * DAY,
                                 status='completed', unique=False,
                                 **({'type': 'dynamic_tool', 'tool': value} if field == 'tool'
                                    else {'command': value})) for field, value in pair]
                tied[1]['ordinal'] = tied[0]['ordinal']

        # Every exit code at its class boundary, on failed and on not-failed commands.
        for code in EXIT_CODES + EXIT_NOT_INTEGER:
            for status in ('failed', 'completed'):
                add_item(w, rng, mains[1], feature[mains[1]['id']], status=status,
                         start=rng.randrange(window['start'], window['end'] - HOUR), length=rand_len(rng), exit=code)
        # Every status and every tool type, and item types that must be ignored.
        for status in STATUSES:
            for t in TOOL_TYPES:
                add_item(w, rng, mains[3], feature[mains[3]['id']], type=t, status=status, length=rand_len(rng))
        for t in IGNORED_TYPES:
            add_item(w, rng, mains[3], feature[mains[3]['id']], type=t, status='completed', length=100)
        # Tool names in both spellings, in any case, and in every shape the reader must refuse.
        for name in NAME_POOL:
            add_item(w, rng, mains[2], feature[mains[2]['id']], type='dynamic_tool', tool=name, length=rand_len(rng))
        # Calls with no run, on the threads providers' own subagents live on.
        for t in [sub_dates, sub_dates_edge, sub_rows, sub_both, plain, unrecorded, dict(id=orphan)]:
            for _ in range(rng.randrange(3, 12)):
                add_item(w, rng, t, None, type=rng.choices(TOOL_TYPES, (50, 20, 10, 10, 10))[0])
        for _ in range(4):
            add_item(w, rng, rng.choice(mains), dict(id='ghost-run'))
        COVER['a sentinel in every text field'] = 1
    elif mode == 'empty':
        COVER['a sentinel in every text field'] = 1
    return w


# ── the reference: the expected `Tools` value, from the records above ─────────

def looks_ident(v):
    return isinstance(v, str) and 0 < len(v) <= 80 and all(c in IDENT for c in v)


def split_name(name):
    """(server, name): `mcp__S__T`, else the part before the last dot; an empty part means
    no server."""
    if name.startswith('mcp__'):
        rest = name[5:]
        s, sep, t = rest.partition('__')
        return (s, t) if sep and s and t else (None, name)
    s, sep, t = name.rpartition('.')
    return (s, t) if sep and s and t else (None, name)


def kind_of(type_, server, name):
    if type_ != 'dynamic_tool':
        return {'command_execution': 'shell', 'file_change': 'edit', 'file_search': 'search', 'web_search': 'web'}[type_]
    if server is not None:
        return 'tool'
    low = (name or '').lower()
    return 'read' if low in READ_NAMES else 'search' if low in SEARCH_NAMES else 'tool'


EXIT_CLASSES = ((0, 'zero'), (1, 'one'), (2, 'two'), (124, 'timed_out'), (126, 'not_runnable'), (127, 'not_found'))


def exits_of(cmds):
    out = dict(known=0, failed_known=0, zero=0, one=0, two=0, timed_out=0, not_runnable=0,
               not_found=0, signal=0, other=0, passed_nonzero=0)
    for c in cmds:
        code = c['exit']
        if code is None:
            continue
        out['known'] += 1
        if not c['failed']:
            out['passed_nonzero'] += code != 0
            continue
        out['failed_known'] += 1
        name = next((n for v, n in EXIT_CLASSES if v == code), None)
        out[name or ('signal' if 129 <= code <= 255 else 'other')] += 1
    return out


def label_of(provider):
    if not isinstance(provider, str) or provider == '':
        return 'unknown'
    return 'claude' if provider == 'claudeAgent' else provider.lower()


def bucket_of(n):
    for i, (_, top) in enumerate(BUCKETS):
        if top is None or n < top:
            return i


def buckets(lengths):
    out = [dict(label=label, calls=0, ms=0) for label, _ in BUCKETS]
    for n in lengths:
        b = out[bucket_of(n)]
        b['calls'] += 1
        b['ms'] += n
    return out


def median(xs):
    xs = sorted(xs)
    if not xs:
        return None
    h = len(xs) // 2
    return xs[h] if len(xs) % 2 else (xs[h - 1] + xs[h]) / 2


def merged(spans):
    total, edge = 0, None
    for a, b in sorted(spans):
        if edge is None or a > edge:
            total += b - a
            edge = b
        elif b > edge:
            total += b - edge
            edge = b
    return total


def note(msg):
    COVER[msg] += 1


def calls_of(w, p):
    """Every tool call that started in the period, found the two ways the spec gives."""
    lo, hi = iso(p['start']), iso(p['until'])
    all_runs = {r['id']: r for r in w.runs}
    runs = {r['id']: r for r in w.runs
            if r['requested'] < hi and (r['completed'] is None or r['completed'] >= lo)}
    by_dates, by_rows = set(), set()
    for t in w.threads:
        if t['rel'] == 'subagent' and isinstance(t['created'], str) and t['created'] < hi \
                and isinstance(t['updated'], str) and t['updated'] >= lo:
            by_dates.add(t['id'])
    for s in w.subs:
        done = None if s['completed'] is ABSENT else s['completed']
        if s['child'] is not None and isinstance(s['started'], str) and s['started'] < hi \
                and (done is None or done >= lo):
            by_rows.add(s['child'])
    children = by_dates | by_rows
    provider_of = {t['id']: t['provider'] for t in w.threads}
    out = []
    run_items = collections.defaultdict(list)
    for rowid, it in enumerate(w.items):
        run_items[it['run']].append((rowid, it))
    ordered = [it for run, items in run_items.items()
               for _, it in (items if run is None else sorted(items, key=lambda item: (item[1]['ordinal'], item[0])))]
    for it in ordered:
        rec = it['payload']
        is_tool = it['type'] in TOOL_TYPES
        began = rec.get('startedAt', ABSENT)
        text = began if isinstance(began, str) else None
        instant = ms(text) if text is not None else None
        in_window = instant is not None and p['start'] <= instant < p['until']
        if is_tool:
            if rec.get('type') in TOOL_TYPES:
                note('a payload type that must not decide the kind')
            STATUS_SEEN.add(it['status'])
            TYPE_SEEN.add(it['type'])
        else:
            note('an item of a type that must be ignored')
        by_run = it['run'] in runs if it['run'] is not None else it['thread'] in children
        if by_run and text is not None:
            at = ms(text)
            if at == p['start']: note('call at exactly the first instant, counted')
            if at == p['start'] - 1: note('call 1 ms before the first instant, left out')
            if at == p['until'] - 1: note('call 1 ms before the end, counted')
            if at == p['until']: note('call at exactly the end, left out')
            if at in (p['start'], p['until']) and text.endswith('.9996Z'):
                note('a bound spelled so its text sorts before it: in or out by its instant')
        if it['run'] is not None and it['run'] in runs and runs[it['run']]['completed'] == lo and in_window:
            note('run closed exactly at the first instant')
        if it['run'] is not None and it['run'] not in runs and in_window:
            note('call in the period whose run is not open, left out')
            if it['run'] == 'ghost-run': note('a run whose id no row has')
            if all_runs.get(it['run'], {}).get('requested') == hi:
                note('run requested exactly at the end')
        if it['run'] is None and it['thread'] not in children and in_window:
            note('no-run call on a thread found by neither route, left out')
        if not is_tool or not in_window or not by_run:
            continue
        if it['run'] is None:
            in_dates, in_rows = it['thread'] in by_dates, it['thread'] in by_rows
            note('no-run call found by both routes' if in_dates and in_rows else
                 'no-run call found by its own dates only' if in_dates else 'no-run call found by a subagents row only')
            thread, run, provider = it['thread'], None, provider_of.get(it['thread'], ABSENT)
        else:
            thread, run, provider = runs[it['run']]['thread'], it['run'], runs[it['run']]['provider']
            if thread != it['thread']:
                note("a call attributed to its run's thread, not its own")
        start = ms(began)
        if start is None:
            continue
        end = ms(rec.get('completedAt', ABSENT))
        if end is None:
            value = rec.get('completedAt', ABSENT)
            if value is ABSENT: note('open call: end absent')
            elif value is None: note('open call: end null')
            elif isinstance(value, bool): pass
            elif isinstance(value, (int, float)): note('open call: end a number')
            elif isinstance(value, str): note('open call: end not a timestamp')
        elif end < start:
            note('open call: end before the start')
        elif end == start:
            note('call of zero length')
        elif end >= p['until']:
            note('call that ends after the period, counted whole')
        raw = rec.get('toolName', ABSENT) if it['type'] == 'dynamic_tool' else ABSENT
        server, name = split_name(raw) if looks_ident(raw) else (None, None)
        if it['type'] == 'dynamic_tool':
            if raw is not ABSENT and not looks_ident(raw): note('tool name not an identifier')
            if isinstance(raw, str) and len(raw) == 80: note('tool name of exactly 80 characters')
            if isinstance(raw, str) and len(raw) == 81: note('tool name of 81 characters')
            if raw == '': note('tool name empty')
            if isinstance(raw, (int, float)) and not isinstance(raw, bool): note('tool name a number')
            if isinstance(raw, str) and server is not None and raw.startswith('mcp__'): note('tool name in mcp__S__T')
            if isinstance(raw, str) and server is not None and not raw.startswith('mcp__'):
                note('tool name in server.name')
            if isinstance(raw, str) and raw != raw.lower() and raw.lower() in READ_NAMES + SEARCH_NAMES:
                note('tool name in mixed case')
            if looks_ident(raw) and name is not None and server is None \
                    and (name.lower() not in READ_NAMES + SEARCH_NAMES):
                note('tool name not on the receiver')
        programs, label = (), None
        if it['type'] == 'command_execution':
            value = rec.get('input', ABSENT)
            if isinstance(value, str) and value in LOOKUP:
                programs, label = LOOKUP[value]
            elif value is not ABSENT:
                note('a command line the list does not know')
            if len(programs) >= 2: note('chain labelled by its weightiest program')
            if not programs and value is not ABSENT: note('command with no listed program')
            for n in programs:
                if label is not None and n != label:
                    note('program that appears in a chain but labels none')
        code = rec.get('exitCode', ABSENT)
        code = code if it['type'] == 'command_execution' and isinstance(code, int) and not isinstance(code, bool) else None
        if code is not None:
            if code in EXIT_TAGS: note(EXIT_TAGS[code])
            note('command marked failed with an exit code' if it['status'] == 'failed'
                 else 'command not marked failed with an exit code')
        elif it['type'] == 'command_execution' and rec.get('exitCode', ABSENT) is not ABSENT:
            note('exit code not an integer')
        if it['type'] != 'command_execution' and 'exitCode' in rec:
            note('exit code on a non-command')
        out.append(dict(kind=kind_of(it['type'], server, name), name=name, server=server, programs=programs,
                        label=label, start=start, end=end, failed=it['status'] == 'failed',
                        provider=label_of(provider), thread=thread, run=run, exit=code, without_run=run is None))
    # A recorded end that is not the call's own: within one thread, by that end exactly.
    groups = collections.defaultdict(list)
    for c in out:
        if c['end'] is not None and c['end'] >= c['start']:
            groups[(c['thread'], c['end'])].append(c)
    for members in groups.values():
        if len(members) < 2:
            note('closed together: a single call at that end, left alone')
            continue
        span = max(c['start'] for c in members) - min(c['start'] for c in members)
        if span > 1000:
            for c in members:
                c['together'] = True
            note('closed together: starts 1001 ms apart' if span == 1001 else
                 'closed together: starts minutes apart')
        else:
            note('closed together: starts 999 ms apart, left alone' if span == 999 else
                 'closed together: starts 1000 ms apart, left alone' if span == 1000 else
                 'closed together: a parallel batch with one start')
    by_end = collections.defaultdict(set)
    for c in out:
        if c['end'] is not None:
            by_end[c['end']].add(c['thread'])
    if any(len(threads) >= 2 for threads in by_end.values()):
        note('closed together: the same end in two threads')
    for c in out:
        KIND_SEEN.add(c['kind'])
        c['length'] = c['end'] - c['start'] if c['end'] is not None and c['end'] >= c['start'] \
            and not c.get('together') else None
    return out


def counted(c):
    return c['length'] is not None


def stats(cs, chained=0, appears_in=0):
    ended = [c for c in cs if counted(c)]
    lengths = [c['length'] for c in ended]
    by = collections.defaultdict(list)
    for c in cs:
        by[c['provider']].append(c)
    shares = []
    for provider, group in by.items():
        mine = [c for c in group if counted(c)]
        shares.append(dict(provider=provider, calls=len(group),
                           ms=sum(c['length'] for c in mine),
                           failed=sum(1 for c in group if c['failed']),
                           closed_together=sum(1 for c in group if c.get('together'))))
    return dict(calls=len(cs), ms=sum(lengths), failed=sum(1 for c in cs if c['failed']),
                open=sum(1 for c in cs if c['end'] is None or c['end'] < c['start']),
                closed_together=sum(1 for c in cs if c.get('together')),
                longest_ms=max(lengths, default=0), median_ms=median(lengths), lengths=buckets(lengths),
                chained=chained, appears_in=appears_in,
                by_provider=sorted(shares, key=lambda s: (-s['calls'], s['provider'])),
                exits=exits_of([c for c in cs if c['kind'] == 'shell']))


def ref_period(w, p, together_rule=True):
    cs = calls_of(w, p)
    if not together_rule:
        for c in cs:
            c.pop('together', None)
            c['length'] = c['end'] - c['start'] if c['end'] is not None and c['end'] >= c['start'] else None
    zone = w.zone
    firsts = [ms(t['created']) for t in w.threads if ms(t['created']) is not None]
    first = min(firsts) if firsts else None
    b = p['days']
    elapsed = sum(1 for i in range(p['n']) if b[i] < p['until'])
    recorded = 0 if first is None else sum(1 for i in range(p['n']) if b[i] < p['until'] and b[i + 1] > first)
    cmds = [c for c in cs if c['kind'] == 'shell']
    ended = [c for c in cs if counted(c)]
    kinds = []
    for kind in ('shell', 'edit', 'read', 'search', 'web', 'tool'):
        mine = [c for c in cs if c['kind'] == kind]
        if mine:
            kinds.append(dict(kind=kind, name=None, **stats(mine)))
    labels = {c['label'] for c in cmds if c['label']}
    appearing = {n for c in cmds for n in c['programs']}
    programs = []
    for name in sorted(labels | appearing):
        mine = [c for c in cmds if c['label'] == name]
        programs.append(dict(kind='shell', name=name, **stats(
            mine, chained=sum(1 for c in mine if len(c['programs']) >= 2),
            appears_in=sum(1 for c in cmds if name in c['programs']))))
    if any(not c['programs'] for c in cmds):
        mine = [c for c in cmds if not c['programs']]
        programs.append(dict(kind='shell', name=None, **stats(mine)))
    groups = collections.defaultdict(list)
    for c in cs:
        if c['kind'] != 'shell':
            groups[(c['kind'], c['server'], c['name'])].append(c)
    tools = [dict(kind=kind, name=name, **({'server': server} if server is not None else {}), **stats(mine))
             for (kind, server, name), mine in groups.items()]
    provs = collections.defaultdict(list)
    for c in cs:
        provs[c['provider']].append(c)
    providers = []
    for provider, group in provs.items():
        s = stats(group)
        providers.append(dict(provider=provider, calls=s['calls'], ms=s['ms'], failed=s['failed'],
                              closed_together=s['closed_together'],
                              commands=sum(1 for c in group if c['kind'] == 'shell'),
                              commands_failed=sum(1 for c in group if c['kind'] == 'shell' and c['failed']),
                              exits=exits_of([c for c in group if c['kind'] == 'shell'])))
    providers.sort(key=lambda row: (-row['calls'], row['provider']))
    named = [c for c in cmds if c['programs']]
    by_thread = collections.defaultdict(list)
    for c in ended:
        by_thread[c['thread']].append((c['start'], c['end']))
    busy = sum(merged(v) for v in by_thread.values())
    longest = sorted(ended, key=lambda c: (-c['length'], c['start']))[:10]
    if sum(c['run'] in w.tie_runs for c in longest) > 1:
        note('longest calls tied on run ordinal and ordered by rowid')
    if len(providers) >= 4: note('four or more providers')
    if any(row['provider'] == 'unknown' for row in providers): note('a provider not recorded')
    if recorded < elapsed: note('days before the first record')
    if not cs: note('a period with no calls')
    if busy < sum(c['length'] for c in ended): note('overlapping calls in one thread')
    if busy > merged([(c['start'], c['end']) for c in ended]):
        note('overlapping calls in two threads')
    return dict(
        period=p['span'], **{'from': zone.date(p['start'])}, to=zone.date(p['last_day']), so_far=p['so_far'],
        days=p['n'], days_elapsed=elapsed, days_recorded=recorded,
        calls=len(cs), ms=sum(c['length'] for c in ended), busy_ms=busy,
        failed=sum(1 for c in cs if c['failed']),
        open=sum(1 for c in cs if c['end'] is None or c['end'] < c['start']),
        closed_together=sum(1 for c in cs if c.get('together')),
        threads=len({c['thread'] for c in cs}), without_run=sum(1 for c in cs if c['without_run']),
        commands=dict(calls=len(cmds), ms=sum(c['length'] for c in cmds if counted(c)),
                      failed=sum(1 for c in cmds if c['failed']), named=len(named),
                      unnamed=len(cmds) - len(named),
                      unnamed_ms=sum(c['length'] for c in cmds if counted(c) and not c['programs']),
                      chained=sum(1 for c in cmds if len(c['programs']) >= 2), listed=None,
                      exits=exits_of(cmds)),
        kinds=kinds, programs=programs, tools=tools, lengths=buckets([c['length'] for c in ended]),
        providers=providers,
        longest=[dict(kind=c['kind'], name=c['label'] if c['kind'] == 'shell' else c['name'],
                      **({'server': c['server']} if c['server'] is not None else {}),
                      ms=c['length'], failed=c['failed'], provider=c['provider'], thread=c['thread'],
                      run=c['run'], started=c['start'], exit=c['exit'],
                      when=zone.when(c['start'], p['span'])) for c in longest],
        min_for_rate=20)


def expected_for(w, opt, together_rule=True):
    which = dict((o[0], o[2]) for o in OPTIONS)[opt]
    last = ref_period(w, w.periods[which], together_rule)
    # product.rs: a last full week or month with no calls shows the current one when that has some.
    if which.startswith('last') and last['calls'] == 0:
        this = ref_period(w, w.periods['this-month' if which == 'last-month' else 'this-week'], together_rule)
        if this['calls'] > 0:
            note('the last period empty, so this one is shown')
            return this
    return last


# ── writing the database ───────────────────────────────────────────────────────

def templates(work):
    out = {}
    for variant in ('main-096', 'replayed-083', 'older-074', 'no-subagent-columns'):
        fixture = 'older-074' if variant == 'no-subagent-columns' else variant
        path = work / ('schema-%s.sqlite' % variant)
        db = sqlite3.connect(path)
        db.executescript((ROOT / 'checks/fixtures' / ('t3-schema-%s.sql' % fixture)).read_text())
        if variant == 'no-subagent-columns':
            for index in ('thread_idx', 'child_thread_idx'):
                db.execute('DROP INDEX IF EXISTS %ssubagents_%s' % (P, index))
            for column in ('child_thread_id', 'started_at', 'completed_at'):
                db.execute('ALTER TABLE %ssubagents DROP COLUMN %s' % (P, column))
        db.commit()
        db.close()
        out[variant] = path
    return out


def write_db(w, path, template):
    shutil.copyfile(template, path)
    db = sqlite3.connect(path)
    old = '2020-01-01T00:00:00.000Z'
    for t in w.threads:
        payload = dict(threadId=t['id'], projectId='project', title='%s title' % SENTINEL,
                       createdAt=t['created'], updatedAt=t['updated'], providerInstanceId=t['provider'])
        if t['rel'] is not None:
            payload['lineage'] = {'relationshipToParent': t['rel']}
        db.execute('INSERT INTO %sthreads(thread_id,project_id,title,default_provider,runtime_mode,'
                   'interaction_mode,created_at,updated_at,archived_at,deleted_at,payload_json) '
                   'VALUES (?,?,?,?,?,?,?,?,?,?,?)' % P,
                   (t['id'], 'project', SENTINEL, 'decoy-column', 'full-access', 'default',
                    t['created'] if isinstance(t['created'], str) else old,
                    t['updated'] if isinstance(t['updated'], str) else old, None, None, json.dumps(clean(payload))))
    for r in w.runs:
        payload = dict(runId=r['id'], status='completed', providerInstanceId=r['provider'], prompt=SENTINEL)
        db.execute('INSERT INTO %sruns(run_id,thread_id,ordinal,provider,status,requested_at,completed_at,payload_json) '
                   'VALUES (?,?,?,?,?,?,?,?)' % P,
                   (r['id'], r['thread'], r['ordinal'], 'decoy-column', r['status'], r['requested'],
                    r['completed'], json.dumps(clean(payload))))
    for it in w.items:
        db.execute('INSERT INTO %sturn_items(turn_item_id,thread_id,run_id,ordinal,type,status,updated_at,payload_json) '
                   'VALUES (?,?,?,?,?,?,?,?)' % P,
                   (it['id'], it['thread'], it['run'], it['ordinal'], it['type'], it['status'], old,
                    json.dumps(clean(it['payload']))))
    for s in w.subs:
        payload = dict(childThreadId=w.threads[0]['id'], startedAt=old, completedAt=None,
                       prompt=SENTINEL, summary=SENTINEL, result=SENTINEL)
        if w.has_sub_columns:
            db.execute('INSERT INTO %ssubagents(subagent_id,thread_id,parent_node_id,provider,child_thread_id,'
                       'origin,status,started_at,completed_at,updated_at,payload_json) VALUES (?,?,?,?,?,?,?,?,?,?,?)' % P,
                       (s['id'], s['thread'], 'node', 'decoy-column', s['child'], 'provider_native', 'completed',
                        None if s['started'] is ABSENT else s['started'],
                        None if s['completed'] is ABSENT else s['completed'], old, json.dumps(clean(payload))))
        else:
            payload['childThreadId'] = s['child']
            payload['startedAt'] = None if s['started'] is ABSENT else s['started']
            payload['completedAt'] = None if s['completed'] is ABSENT else s['completed']
            db.execute('INSERT INTO %ssubagents(subagent_id,thread_id,parent_node_id,provider,origin,status,'
                       'updated_at,payload_json) VALUES (?,?,?,?,?,?,?,?)' % P,
                       (s['id'], s['thread'], 'node', 'decoy-column', 'provider_native', 'completed', old,
                        json.dumps(clean(payload))))
    db.commit()
    db.close()


# ── comparing ──────────────────────────────────────────────────────────────────

def value_kind(v):
    return 'null' if v is None else 'bool' if isinstance(v, bool) else 'text' if isinstance(v, str) \
        else 'number' if isinstance(v, (int, float)) else type(v).__name__


def first_difference(a, b, path='$'):
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a: return '%s.%s: nothing expected, program has %r' % (path, k, b[k])
            if k not in b: return '%s.%s: expected %r, the program has no such key' % (path, k, a[k])
            d = first_difference(a[k], b[k], '%s.%s' % (path, k))
            if d: return d
        return None
    if isinstance(a, list) and isinstance(b, list):
        for i, (x, y) in enumerate(zip(a, b)):
            d = first_difference(x, y, '%s[%d]' % (path, i))
            if d: return d
        return None if len(a) == len(b) else '%s: expected %d items, program has %d' % (path, len(a), len(b))
    if value_kind(a) != value_kind(b) or a != b:
        return '%s: expected %r, program has %r' % (path, a, b)
    return None


def spec_key(row):
    return (-row['ms'], -row['calls'], row['name'] is None, row['name'] or '')


def total_key(row):
    return (row['kind'], row.get('server') or '', row['name'] is not None, row['name'] or '')


def long_key(row):
    return (-row['ms'], row['started'], row['kind'], row.get('server') or '', row['name'] is not None,
            row['name'] or '', row['provider'], row['thread'], row['run'] or '')


def compare(expected, got, listed, tie_runs=()):
    """The first difference between the expected value and the program's JSON, or None.
    Rows that tie on the spec's sort key may come in any order; the key itself is checked."""
    if not isinstance(got, dict):
        return 'the program did not print a JSON object'
    expected = json.loads(json.dumps(expected))
    got = json.loads(json.dumps(got))
    commands = got.get('commands')
    if isinstance(commands, dict) and 'listed' in commands:
        n = commands['listed']
        if listed.get('n') is None:
            if not (isinstance(n, int) and not isinstance(n, bool) and n >= DISTINCT_PROGRAMS):
                return '$.commands.listed: %r is not a count of at least the %d programs of the fixture table' \
                       % (n, DISTINCT_PROGRAMS)
            listed['n'] = n
        elif listed['n'] != n:
            return '$.commands.listed: %r here, %r in an earlier run' % (n, listed['n'])
        expected['commands']['listed'] = listed['n']
    for field in ('kinds', 'programs', 'tools'):
        rows = got.get(field)
        if not isinstance(rows, list):
            return '$.%s is not a list' % field
        if [spec_key(r) for r in rows] != sorted(spec_key(r) for r in rows):
            return '$.%s is not in the order the spec gives (ms down, calls down, name with None last)' % field
        d = first_difference(sorted(expected[field], key=total_key), sorted(rows, key=total_key), '$.' + field)
        if d: return d
        expected.pop(field), got.pop(field)
    longest = got.get('longest')
    if isinstance(longest, list) and isinstance(expected.get('longest'), list):
        if [(-r['ms'], r['started']) for r in longest] != sorted((-r['ms'], r['started']) for r in longest):
            return '$.longest is not in the order the spec gives (ms down, then started)'
        want_ties = [r for r in expected['longest'] if r['run'] in tie_runs]
        got_ties = [r for r in longest if r['run'] in tie_runs]
        d = first_difference(want_ties, got_ties, '$.longest by tied ordinal and rowid')
        if d: return d
        expected['longest'].sort(key=long_key)
        longest.sort(key=long_key)
    return first_difference(expected, got)


def strip_fields(v, keys):
    if isinstance(v, dict): return {k: strip_fields(x, keys) for k, x in v.items() if k not in keys}
    if isinstance(v, list): return [strip_fields(x, keys) for x in v]
    return v


# ── running the program ────────────────────────────────────────────────────────

class Ctx:
    def __init__(self, work):
        self.work, self.failures, self.checks, self.runs, self.listed = work, [], 0, 0, {}
        self.cache_hits = 0

    def run(self, args, tz='UTC', extra=None, raw=False):
        env = {'PATH': os.environ['PATH'], 'T3CODE_HOME': str(self.work / 'empty-t3-home'),
               'HOME': str(self.work / 'home'), 'XDG_CACHE_HOME': str(self.work / 'xdg-cache'),
               'TZ': tz, 'NO_COLOR': '1'}
        for name in ('T3_AGENTS_READ_THREADS', 'T3_AGENTS_SKIM', 'T3_AGENTS_NO_CACHE',
                     'T3_AGENTS_CACHE', 'T3_AGENTS_CACHE_STATS'):
            if name in os.environ:
                env[name] = os.environ[name]
        env.update(extra or {})
        self.runs += 1
        return subprocess.run([str(BINARY)] + [str(a) for a in args], env=env, cwd=ROOT,
                              capture_output=True, text=not raw, timeout=120)

    def fail(self, label, detail):
        self.failures.append((label, detail))
        print('FAIL %s\n     %s' % (label, detail))


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def check_case(ctx, name, seed, w, db, together_rule):
    before, listing = digest(db), sorted(os.listdir(db.parent))
    failures, calls = len(ctx.failures), 0
    reference = None
    for opt, flags, _ in OPTIONS:
        label = 'seed %s / %s / %s / %s' % (seed, name, w.zone.name, opt)
        for extra, json_mode in ((('--json',), True), ((), False)):
            raw = json_mode and opt == 'none' and together_rule
            r = ctx.run(['--tools', *extra, '--db', db, '--now-ms', int(w.now), *flags], tz=w.zone.name,
                        extra=CACHE_OFF if raw else None, raw=raw)
            if raw: reference = r
            stdout, stderr = (r.stdout.decode(), r.stderr.decode()) if raw else (r.stdout, r.stderr)
            if r.returncode != 0:
                ctx.fail(label + ('' if json_mode else ' (plain list)'),
                         'exit %d, stderr: %s' % (r.returncode, stderr.strip()[:400]))
                continue
            if leaked(stdout + stderr, prose=not json_mode):
                ctx.fail(label + ('' if json_mode else ' (plain list)'), '"never" appears in the output')
            if not json_mode:
                if not stdout.strip():
                    ctx.fail(label + ' (plain list)', 'printed nothing')
                continue
            try:
                got = json.loads(stdout)
            except ValueError as e:
                ctx.fail(label, 'stdout is not JSON: %s' % e)
                continue
            want = expected_for(w, opt, together_rule)
            calls = max(calls, want['calls'])
            if not together_rule:
                want, got = strip_fields(want, ('closed_together',)), strip_fields(got, ('closed_together',))
            d = compare(want, got, ctx.listed, w.tie_runs)
            if d: ctx.fail(label, 'first difference at ' + d)
    if digest(db) != before:
        ctx.fail('seed %s / %s' % (seed, name), 'the database file changed')
    if sorted(os.listdir(db.parent)) != listing:
        ctx.fail('seed %s / %s' % (seed, name),
                 'files appeared beside the database: %s' % sorted(set(os.listdir(db.parent)) - set(listing)))
    ctx.checks += 1
    if len(ctx.failures) == failures:
        print('ok   %-22s %-10s %-18s %-20s %4d items, up to %4d calls a period' %
              (name, seed.split(':')[0], w.variant, w.zone.name, len(w.items), calls))
    return reference


CACHE_OFF = {'T3_AGENTS_CACHE': '0', 'T3_AGENTS_CACHE_STATS': '0'}


def check_cache(ctx, seed, w, db, reference):
    args = ['--tools', '--json', '--db', db, '--now-ms', int(w.now)]
    on = {'T3_AGENTS_CACHE': '1', 'T3_AGENTS_NO_CACHE': '0', 'T3_AGENTS_CACHE_STATS': '1'}

    def read(settings):
        return ctx.run(args, tz=w.zone.name, extra=settings, raw=True)

    def agree(label, reference, candidate):
        if reference.returncode or candidate.returncode or reference.stdout != candidate.stdout:
            ctx.fail('seed %s / cache %s' % (seed, label),
                     'cache-off exit %d, cache-on exit %d, stdout equal: %s, stderr: %r' %
                     (reference.returncode, candidate.returncode, reference.stdout == candidate.stdout,
                      candidate.stderr[:400]))

    agree('first fill', reference, read(on))
    warm = read(on)
    agree('warm', reference, warm)
    hits = re.search(rb'^t3-agents: cache: (\d+) of \d+ finished runs from the cache, \d+ written$', warm.stderr, re.M)
    if hits:
        ctx.cache_hits += int(hits[1])

    rng = random.Random(seed + ':cache-write')
    source = sqlite3.connect(db)
    try:
        with source:
            if w.items:
                item = rng.choice(w.items)
                start = ms(item['payload'].get('startedAt'))
                end = iso((w.now if start is None else start) + rng.randrange(1000, HOUR))
                source.execute("UPDATE %sturn_items SET payload_json=json_set(payload_json,'$.completedAt',?) "
                               "WHERE turn_item_id=?" % P, (end, item['id']))
            else:
                source.execute('PRAGMA user_version=%d' % rng.randrange(1, 65536))
    finally:
        source.close()
    before = digest(db)
    agree('after source write', read(CACHE_OFF), read(on))
    if digest(db) != before:
        ctx.fail('seed %s / cache after source write' % seed, 'the database file changed during the read')


def hostile(ctx):
    """What must never happen, on databases that are missing, not databases, or have no tables."""
    junk = ctx.work / 'not-a-database.bin'
    junk.write_bytes(b'this is not sqlite, and it says "never read"\n' * 20)
    empty = ctx.work / 'no-t3-tables.sqlite'
    sqlite3.connect(empty).close()
    cases = (('a missing database', ctx.work / 'does-not-exist.sqlite'),
             ('a file that is not a database', junk), ('a database with no T3 tables', empty))
    for what, path in cases:
        before = digest(path) if path.exists() else None
        for extra, tag in ((('--json',), ' --json'), ((), '')):
            r = ctx.run(['--tools', *extra, '--db', path, '--now-ms', 1795000000000])
            if leaked(r.stdout + r.stderr, prose=False): ctx.fail(what + tag, '"never" appears in the output')
            if r.returncode == 0: ctx.fail(what + tag, 'exit 0 where the database cannot be read')
            if r.stdout.strip(): ctx.fail(what + tag, 'wrote to stdout: %r' % r.stdout[:200])
            if not r.stderr.strip(): ctx.fail(what + tag, 'printed no advice on stderr')
        if before is not None and digest(path) != before: ctx.fail(what, 'the file changed')
        print('ok   %-32s rejected, nothing on stdout, no "never", file untouched' % what)
    r = ctx.run(['--tools', '--json', '--demo'], extra={'T3_AGENTS_NO_THEMES': '1'})
    if r.returncode != 2 or "Unknown option '--tools'" not in r.stderr:
        ctx.fail('T3_AGENTS_NO_THEMES=1 --tools',
                 'exit %d, stderr %r (wanted exit 2 and "Unknown option" for --tools)' % (r.returncode, r.stderr[:300]))
    else:
        print('ok   T3_AGENTS_NO_THEMES=1 --tools  rejected as an unknown option, exit 2, nothing on stdout')


def local_instant(tz, text):
    return int(datetime.fromisoformat(text).replace(tzinfo=ZoneInfo(tz)).timestamp()) * 1000


SCENARIOS = (  # name, zone, the local instant `now`, how the world is built
    ('la-dst-fall', 'America/Los_Angeles', '2026-11-04T12:00:00', 'wide'),
    ('la-dst-fall-night', 'America/Los_Angeles', '2026-11-01T03:30:00', 'wide'),
    ('la-dst-spring', 'America/Los_Angeles', '2026-03-10T12:00:00', 'wide'),
    ('la-dst-spring-day', 'America/Los_Angeles', '2026-03-08T04:00:00', 'wide'),
    ('utc-first-instant', 'UTC', '2026-11-16T00:00:00', 'wide'),
    ('utc-month-first-instant', 'UTC', '2026-12-01T00:00:00', 'wide'),
    ('kolkata-midweek', 'Asia/Kolkata', '2026-11-18T10:00:00', 'wide'),
    ('berlin-dst-end', 'Europe/Berlin', '2026-10-28T09:00:00', 'wide'),
    ('auckland-dst-start', 'Pacific/Auckland', '2026-09-30T09:00:00', 'wide'),
    ('only-this-period', 'UTC', '2026-11-18T10:00:00', 'this-only'),
    ('only-last-period', 'America/Los_Angeles', '2026-03-10T10:00:00', 'last-only'),
    ('only-this-month', 'Asia/Kolkata', '2026-11-18T10:00:00', 'this-only'),
    ('no-rows', 'UTC', '2026-11-18T10:00:00', 'empty'),
)
ZONES = ('UTC', 'America/Los_Angeles', 'Asia/Kolkata', 'Europe/Berlin', 'Pacific/Auckland')
VARIANTS = ('main-096', 'replayed-083', 'older-074', 'no-subagent-columns')


def random_now(rng, tz):
    zone = Zone(tz)
    now = int(datetime(2026, 1, 5, tzinfo=timezone.utc).timestamp()) * 1000 + rng.randrange(0, 357 * 86400) * 1000
    if rng.random() < 0.5:   # an instant at, or a minute either side of, a period's first one
        anchor = period(now, rng.choice(('ThisWeek', 'ThisMonth')), zone)['start']
        return int(anchor) + rng.choice((-1000, -1, 0, 1, 60_000, 26 * HOUR, 7 * HOUR + 30 * 60_000))
    return now


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--seed', type=int, default=1)
    ap.add_argument('--cases', type=int, default=28, help='random databases besides the fixed ones (default 28)')
    ap.add_argument('--only', help='run only the case with this name, e.g. random-3 or la-dst-fall')
    ap.add_argument('--zones', help='comma-separated zones for the random databases (default: %s)' % ', '.join(ZONES))
    ap.add_argument('--without-closed-together', action='store_true',
                    help='compare against a build that does not have the closed-together rule yet')
    ap.add_argument('--keep', action='store_true', help='keep the databases under checks/generated/tools-check/')
    args = ap.parse_args()
    together_rule = not args.without_closed_together
    if not BINARY.exists():
        sys.exit('build first: cargo build --release --locked (%s is missing)' % BINARY)
    base = ROOT / 'checks/generated/tools-check'
    base.mkdir(parents=True, exist_ok=True)
    work = base / ('run-%d-%d' % (os.getpid(), int(time.time())))
    work.mkdir()
    (work / 'empty-t3-home').mkdir()
    (work / 'home').mkdir()
    (work / 'xdg-cache').mkdir()
    started = time.time()
    zones = args.zones.split(',') if args.zones else list(ZONES)
    plan = [(name, tz, local_instant(tz, at), mode) for name, tz, at, mode in SCENARIOS]
    for i in range(args.cases):
        rng = random.Random('%d:now:%d' % (args.seed, i))
        tz = zones[i % len(zones)]
        plan.append(('random-%d' % i, tz, random_now(rng, tz), 'wide'))
    ctx = Ctx(work)
    try:
        schemas = templates(work)
        hostile(ctx)
        for n, (name, tz, now, mode) in enumerate(plan):
            if args.only and name != args.only:
                continue
            variant = VARIANTS[n % len(VARIANTS)]
            seed = '%d:%s' % (args.seed, name)
            world = build_world(random.Random(seed), variant, tz, now, mode)
            db = work / ('%s.sqlite' % name)
            write_db(world, db, schemas[variant])
            reference = check_case(ctx, name, seed, world, db, together_rule)
            if not args.without_closed_together:
                check_cache(ctx, seed, world, db, reference)
            if not args.keep:
                db.unlink()
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)
    if not args.only:
        if sys.platform.startswith('linux') and not args.without_closed_together and not ctx.cache_hits:
            ctx.fail('cache coverage', 'no decoded entries were used on a warm read')
        for status in STATUSES:
            if status in STATUS_SEEN: COVER['status ' + status] += 1
        if len(STATUS_SEEN) == len(STATUSES): COVER['every status'] = len(STATUS_SEEN)
        if len(TYPE_SEEN) == len(TOOL_TYPES): COVER['every tool type'] = len(TYPE_SEEN)
        COVER.require(REQUIRED_COVER)
        print('\nwhat the expected values exercised:')
        for key, count in sorted(COVER.items()):
            print('  %6d  %s' % (count, key))
    print('\n%d databases, %d program runs, %.1fs%s, seed %d: %s' %
          (ctx.checks, ctx.runs, time.time() - started,
           '' if together_rule else ' (closed together left out)', args.seed,
           'FAILED' if ctx.failures else 'all fields agree'))
    if ctx.failures:
        print('%d differences; first: %s: %s' % (len(ctx.failures), *ctx.failures[0]))
        sys.exit(1)


if __name__ == '__main__':
    main()
