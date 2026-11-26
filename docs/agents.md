# Using Loupe from an agent

Loupe can tell another program what T3 Code's agents are doing, and how they
did. That program is often a coding agent: one asked "is anything waiting on
me?", "why was that task slow?" or "which commands keep failing?". This page
lists the commands that print something a program can parse, what the fields
mean, how a failure shows up, and what the numbers can and can't support.

[AGENTS.md](../AGENTS.md) covers installing Loupe and working on its code.

## Ground rules

- Every command on this page only reads. None of them changes T3 Code's data,
  and none of them sends anything anywhere.
- Try a command with `--demo` first. It prints made-up agents in the same shape
  as the real output. Never report demo output as the person's.
- Let Loupe read T3's database. Don't open, copy or query `statev2.sqlite`
  yourself. Loupe keeps prompts, message text, tool output and reasoning out of
  its results, and a direct query doesn't.
- The output holds thread titles, model names and program names. Treat it as
  the person's private data. Don't put it in a public issue or send it to an
  outside service without asking.
- Read the whole output. Redirect it to a file instead of piping it into
  `head`: a pipe closed early makes Loupe exit with an error.

## Which command answers which question

| Question | Command | Prints |
| --- | --- | --- |
| Is anything running, or waiting on the person? | `loupe --status` | one line of JSON |
| Tell me each time that changes | `loupe --status --watch` | one JSON line per change, until stopped |
| What is every agent doing? | `loupe --once` | plain text, one row per agent |
| How much did agents work, and on what? | `loupe --recap --json` | JSON |
| Which tools and commands ran, and which failed? | `loupe --tools --json` | JSON |
| Where did one task's time go, or where did it stop? | `loupe --trace [ID] --json` | JSON |
| Does T3 Code send telemetry, and do the opt-outs work? | `loupe --telemetry-check --json` | JSON |

Recap and Tools cover the last full week. Add `--this-week`, `--month` or
`--this-month` for another period. `--trace` without an `ID` picks the latest
task. An `ID` is a run id, a thread id, or the end of a thread id as the Tools
report prints it.

Loupe reads `~/.t3/userdata/statev2.sqlite` unless `T3CODE_HOME` names another
folder or `--db PATH` names the file. `--telemetry-check` reads T3's data
folder, not the database, and refuses `--db`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Finished. `--status` exits 0 even when it can't read the database (see `error` below), and `--telemetry-check` exits 0 whatever its verdict. |
| 1 | It couldn't read T3's data, or the `--trace` id matched nothing. The reason is on stderr. A failed write, such as a closed pipe, also ends here. |
| 2 | The command line was wrong: an unknown option, options that don't go together, or a theme it can't use. The usage is on stderr. |

Results go to stdout and problems to stderr, so stdout is empty when a read
fails.

## `--status`

The line the Omarchy bar widget reads. From the demo, cut to one agent:

```json
{"v":1,"running":10,"waiting":1,"failed":2,"agents":[{"title":"Plan the search index rebuild","state":"waiting","ctx":0.54,"model":"opus 5.5","family":"claude","now":"ask","openAsks":1,"estimatedAsks":0}],"openAsks":1,"estimatedAsks":0}
```

| Field | Meaning |
| --- | --- |
| `v` | The format's version, 1. Fields have been added under version 1, so ignore any you don't know. |
| `running` | Agents with a run in progress, queued and starting ones included. |
| `waiting` | Agents that need the person. |
| `failed` | Failures recorded across all agents in the last ten minutes. |
| `agents` | At most eight: the ones waiting first, then the running ones, newest first in each group. |
| `openAsks` | Recorded questions, secret cards and link cards still open, across every agent. |
| `estimatedAsks` | Questions to the person estimated from agent replies since the person last wrote, counted once per thread. |
| `error` | Only when the database can't be read: a short reason. Every count is 0 then. |

Each agent in `agents`:

| Field | Meaning |
| --- | --- |
| `title` | The thread's title, cut to 80 characters. |
| `state` | `waiting` when it needs the person, otherwise its run status, such as `running`, `queued` or `starting`. |
| `ctx` | The share of its context window in use, from 0 to 1, or `null` when T3 hasn't recorded it. |
| `model` | The short model name. |
| `family` | `claude`, `openai`, `grok`, `gemini` or `other`. |
| `now` | What it's doing: `shell`, `read`, `edit`, `web`, `agent`, `ask`, `think`, `reply`, `you`, `note`, `tool` or `other`. `null` between steps. |
| `openAsks`, `estimatedAsks` | As above, for this agent. |

## The `--json` reports

`--telemetry-check --json` carries `"schema": 1`. The Recap, Tools and Trace
JSON have no version field: their fields follow the reports and can change
between releases, so read the fields you need by name and don't fail on new
ones. [Features](features.md) explains what each number counts.

- **Recap** (`--recap --json`): `period`, `from` and `to` (local dates),
  `so_far` (true for a period still running), and hours as decimal agent
  hours. Two agents at work for an hour are two agent hours. A day outside
  T3's records is `null`, not zero.
- **Tools** (`--tools --json`): the same period fields, then counts, and
  durations in milliseconds (`ms`, `busy_ms`). `commands.exits` breaks
  failures down by exit code where the provider recorded one.
- **Trace** (`--trace --json`): one task. Times such as `requested`, `ended`,
  `now` and each span's `start` and `end` are Unix times in milliseconds.
  `ended` is `null` while the task runs. `split` gives the task's time in
  milliseconds: `queue`, `model`, `tools`, `waiting` (on the person), `between`
  and `total`.
- **Telemetry Check** (`--telemetry-check --json`): `verdict` is one of `off`,
  `blocked`, `partial`, `on`, `misconfigured`, `restart_needed` or
  `unverified`. `summary` says it in a sentence. Each of `findings` has an
  `id`, a `severity`, a `message` and usually a `fix`. `source` is `demo` for
  made-up data. `network` and `writes` are always `none`.

## Reading the numbers right

- **Recorded asks and estimated asks are different things.** `openAsks` counts
  questions T3 recorded. `estimatedAsks` is a guess from the wording of agent
  replies. Say which one you mean.
- **A failed command isn't always broken.** Exit 1 from `rg` or `grep` means it
  found nothing. Only some providers record exit codes, so a provider with no
  failures on record may simply not record them.
- **`--status` is one moment.** Use `--status --watch` to follow it, or ask
  again. The widget asks every two seconds.
- **A period still running isn't comparable to a full one.** Check `so_far` and
  `days_recorded` before comparing two Recaps or two Tools reports.
- **No record isn't zero.** T3 prunes old records, and Recap marks the days it
  has none of as `null`.

## Evaluating agent work with Loupe

The reports are evidence about how agents worked, and they can back up a
review of a run, a model or a workflow.

- To see where one task's time went, use Trace's `split` and `spans`: time in
  the queue, the model thinking, tools running, and the person's replies.
- To find commands that fail often, use Tools' `programs` and
  `commands.exits`. Always give a rate with its count.
- To compare periods, use Recap: agent hours, how tasks ended, models, lines
  changed and pull requests.

What Loupe can't tell you: it never reads prompts, replies, tool output or
the content of files. It can say that a command failed and with which exit
code, but not why. It can say a task took 40 minutes, but not whether the
result was any good. Judge quality from the work itself, and use Loupe for the
when, how long and how often.
