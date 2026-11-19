# Features, in detail

The short list is in the [README](../README.md). This is the long one.

## The dashboard

- **Themes:** 27 built in, including all 23 of Omarchy's, plus Frontier, Paper,
  Phosphor and Dracula. On Omarchy it follows the live theme; elsewhere it
  follows the terminal between Frontier and Paper. `c` and `C` step through
  them and remember the choice. Every built-in keeps the provider colors,
  "needs you" and the warning apart for colorblind readers. A theme of your own
  is an Omarchy `colors.toml` plus a few optional keys, and `--theme-check`
  says how it reads. See [themes](themes.md).

- **Big numbers up top:** active agents, agents waiting on you, tool calls per
  minute, peak context, tokens today, failed calls. Only numbers worth a look
  are big: zeros stay small and dim, and a missing value is a small `—` with the
  reason. If T3 can't be read, the tiles say so instead of showing zeros. In a
  window at least 44 rows tall and about 190 columns wide, every big number is
  drawn in a shadowed block font; smaller windows get compact half-block digits.
  One screen never mixes the two.
- **Needs you, one key away:** the tile shows what's being asked and how long
  it has waited. `n` (or a click on the tile) jumps to the next agent waiting
  on you, and `Enter` opens it in T3. Its `◆` blinks until you've seen it.
- A running thread can leave a question open. Its row keeps the running mark
  and shows how many recorded asks are open. Select it to see whether the run
  is waiting for input or can keep working. A separate `~2`, for example,
  means the agent appears to have asked you two things in a reply.
  `n` visits stopped runs first and skips estimates.
- **Limits:** a meter per subscription window from Omarchy's usage records,
  with a tick for how much of the window has passed, so you can tell when
  you're burning ahead of pace. In short windows it folds into one line with
  each provider's fullest window.
- **Every agent and subagent as a tree:** status, a context meter that warms
  past 75% and goes hot past 90%, what it's doing right now, model, how long
  its latest run took (counting up while it runs), and an activity lane on a
  shared time axis, where a failed call shows as `×`. Select an agent and the
  axis ticks the clock time its run started and ended. A new tool call sweeps
  once across the agent's name.
- **Long threads stay short:** under each agent the list keeps the subagents
  that are running, any waiting on you, and the three latest finishes from the
  last 3 hours. The rest fold into one `┄ 27 earlier ▸ · 25m–7h ago · 2 ✗`
  row, and its lane still shows their activity. Select it to see them listed in
  the details; `→`, `Enter` or a click puts them all back in the list. Top-level
  threads never fold.
- **Three ways to scope the list:** `a` steps from every unarchived thread to
  active only (running or waiting on you), then to T3's sidebar: the threads
  you haven't settled or snoozed, with their subagents. That last one keeps up
  with T3, so settling or snoozing a thread there takes it off here. Anything
  running or waiting on you stays listed in every scope, and the T3 scope also
  keeps the thread open in T3, settled or not.
- **Stall detection:** a running agent with no tool call in flight and nothing
  new for 2 minutes stops spinning and says `◌ quiet 4m · last ❯ npm test`;
  after 8 it says `◌ stalled` and turns warm. A tool call shows its running
  time, and past 5 minutes gets a `△` and turns warm. An agent waiting on its
  own subagents is delegating, not stuck.
- **Nothing hidden without a cue:** a list or panel with more than fits says
  `▼ N more`, and the list's cue sums up what's out of view (`◆1 · 1 stalled ·
  1 ✗`), turning hot when someone waiting on you is scrolled away.
- **Linked to T3, both ways:** open a thread in T3 and the dashboard selects it
  and marks it and its subagents. Press `Enter` on any agent and T3 jumps to
  that thread.
- **Details for the selected agent:** how long its latest run took and when
  (`10m 00s  14:02 → 14:12 · 31m ago`), context as a big number, its growth per
  turn, the last call's token mix, a prompt-cache meter that drains in real
  time, the plan's progress, tool mix, and recent activity. Native subagents
  also show activity when T3 records no application run for them.
- **Failures you can inspect:** recent failed calls stay at the top of the
  details, with the command or tool, age, and exit code when reported. They
  cover the selected lane window across runs, so a later run or successful
  call won't bury them. Scroll the details with `J` / `K` or the wheel;
  `Enter` opens the thread in T3 for full output. A failed call does not by
  itself mean the agent's task failed; even a search with no matches can
  return a nonzero exit code.
- Press `s` for Storage. On Linux it measures worktrees and agent scratch in
  the background, with a subtotal for `target` and `node_modules`. It groups
  settled and gone folders and shows unassigned sizes in Other.
- When a watched volume falls below 10% free or 1 GiB available, a single
  line above the footer says how much is left. It identifies temporary space
  backed by memory. The demo uses example data.

## Recap

`t3-agents --recap` prints one card: the last full week of agent work, Monday
to Sunday. T3 shows you one thread at a time. This is the week across all of
them. `t3-agents --recap --demo` shows a made-up week.

- **Agent hours,** with the week beside them, a column a day. Agent hours are
  the time runs were open, plus the time a thread sat waiting on you after its
  run had ended, from when it asked to when you answered. Five agents working
  for an hour are five agent hours, and two threads waiting on you for an hour
  are two. An ask you never came back to adds nothing after its run.
- **Where the hours went,** in three parts that add up: a tool call running,
  the model thinking and writing, and waiting on you. Waiting on you is an
  approval, or a question the agent cannot go on without; a question it can
  work past does not count. A request for a secret counts while its run is
  open. A request for a link counts while none of the thread's runs is
  working, until it is done or you write in that thread. Waiting on you is the
  one hot color on the card, as it is on the dashboard.
- **Tasks:** how many you started, how many another agent handed over, and
  how many finished. A run that only continues an earlier one is not counted
  as a task.
- **Models,** by the time their runs took, in each provider's color. It is a
  record of what you used. Nothing here ranks them: T3 doesn't record whether
  the work was any good, so the card doesn't pretend to know.
- **Output:** lines added and removed, pull requests merged. Generated files
  count as lines too.
- **Handed to agents:** how often an agent passed work to another, how long
  those took, and how many did not finish: failed, stopped or cancelled.
- **Tokens,** where the provider reports them. When one doesn't, the card says
  how many hours of its runs the total leaves out.
- **Up to three findings,** each a number, a plain sentence, and what it rests
  on: tasks stopped on a plan limit, the time agents waited on you, failures
  and their causes, threads that ran out of room. A finding below its
  threshold says nothing, and a rate is never shown without its count.

Good to know:

- **It reads T3's records, and nothing is sent.** The card is worked out from
  T3's own records, read-only, so it covers what T3 still holds. Numbers for
  finished runs may be cached on your machine so a later Recap reads less;
  [how it works](how-it-works.md) says what and where. The header says how
  many of the week's days have records.
- **It names no thread, project or file.** Numbers, model names and dates
  only, so a screenshot is safe to share.
- **It follows your theme,** and `--theme NAME` picks one. It needs 80 columns.
  A narrower terminal gets a short list, 39 columns wide, with the same numbers
  and a one-row mark for each day. `--narrow` asks for that list whatever the
  terminal's size. Piped, or with `NO_COLOR` set, it is plain text and the bars'
  parts are told apart by their fill.

### The wide card

In a terminal of 120 columns by 40 rows or more, `--recap` prints a wider card
with room for more of the same week. `--wide` asks for it whatever the
terminal's size, `--compact` asks for the 80-column card, and `--narrow` asks
for the short list. A card asked for in a narrower terminal has its lines
wrapped by the terminal, and Recap says so under it. The list does the same
when the terminal is under 39 columns.

- **Agents at work, by hour:** seven days of 24 hours in your local time,
  shaded by how many agents were at work through each hour, on average. T3
  never shows you when your agents worked; this is where the overnight runs
  show up. Each day's hours are beside it, and the seven add up to the week.
- **Waiting on you, in detail:** how many asks held an agent up, a typical
  wait and the longest. Questions an agent could work past are left out.
- **Models as a table:** hours, runs and the tokens each one read.
- **Handed to agents:** which model handed work to which, the three commonest
  pairs.
- **How tasks ended,** with the causes of the failures; tool calls and how many
  were commands; how often a thread was compacted to make room. A command
  counts as failed on any exit code but 0, which a search that finds nothing
  also has, and the card says so: it never reads what a command was.
- **How this is counted,** at the foot, so a shared screenshot explains its own
  numbers. The wide card has this where the smaller one has its findings.
- **`--this-week`** shows the current week so far. When last week has no
  records and this week does, that is what you get anyway.
- **`--json`** prints the numbers, for a script or an agent. A day before T3's
  records begin, or one that has not come yet, is `null` there, not zero.
- **Times are local.** A week starts on Monday at midnight where you are.

### A month

`--recap --month` prints the same two cards for the last full calendar month,
and `--recap --this-month` for the current month so far. Every number means
what it means for a week, over the longer span. Where a week shows its days, a
month shows its weeks, and on the wide card a calendar.
`t3-agents --recap --month --demo` shows a made-up one.

- **On the 80-column card,** the hours have a column for each week beside
  them, where a week has a column for each day. Under each is the date that
  week begins on. A month seldom starts on a Monday, so the first and last
  weeks can be short ones.
- **On the wide card,** a calendar: every day with its date, shaded by its
  agent hours, and each week's hours beside it. The busiest day and the
  busiest week are in bold.
- **A shade is a share of the busiest day:** under a third of its hours, up to
  two thirds, or more. The key writes those out in hours, so the card means
  the same to whoever you show it to.
- **How many days agents ran,** and the agent hours of a typical one: the
  middle one, so a single very long day doesn't move it.
- **A day with no records is not a day without work.** One before T3's records
  begin, or one that has not come yet, keeps its date and gets no shade, and
  a week of such days gets no column.
- **When last month has no records and this month does,** `--month` shows this
  month so far.

## Tools

`t3-agents --tools` prints what agents ran in the last full week, Monday to
Sunday, and what failed. T3 shows tool calls one at a time, inside a
conversation. This is every thread at once. `t3-agents --tools --demo` shows a
made-up week.

- **Calls, call time and failed,** as three tiles. A call is a command, a file
  change, a search or a named tool; an agent handing work to another isn't
  one. Calls overlap, so call time is more than the clock time, and the notes
  at the foot give both. Failed is the provider's own mark.
- **How long they took,** in five lengths. Most calls are over in under a
  second, and a few long ones hold most of the time. That's why every row
  shows both a count and a time: one says what agents do all day, the other
  where it went.
- **By kind:** commands, file changes, searches, web searches, reads and other
  tools. A named tool is a read or a search only when it has a well-known
  name, like `Read` or `Grep`. The rest are other tools, whatever they sound
  like.
- **Commands by program.** A chained line, like `cd app && cargo test | tail`,
  is one call, named for its main program, `cargo`. Its time and failure go
  to `cargo` and nobody else. The last line, "also inside other command
  lines," counts the programs that ran in a line named for another. A command
  with none of the 75 programs on the list is "not on the list."
- **Why commands failed,** by exit code. Exit 1 from `rg` or `grep` is "found
  nothing," an answer more than a breakage. Only some providers record an exit
  code. The report says how many failed commands have one, and a row for the
  rest names whose they are. A provider that records none isn't one whose
  commands didn't fail.
- **Tools by name,** as the provider recorded them. File changes and searches
  have no name, so they're one line underneath. A tool that waits for you, like
  a question it asks, is a call like any other: your wait is in its time.
- **Providers:** the commands each ran and how many it marked failed, as two
  plain counts. There's no ranking, because each provider decides for itself
  what failed means.
- **The longest calls,** with how each ended and the last 8 characters of its
  thread's id: the end of the id, not the start.
- **How this is counted,** at the foot, so a shared screenshot explains its own
  numbers.

Good to know:

- **It reads T3's records, and nothing is sent.** The report is worked out
  from T3's own records, read-only, so it covers what T3 still holds. Numbers
  for finished runs may be cached on your machine;
  [how it works](how-it-works.md) says what and where. The header says how
  many of the week's days have records.
- **Tools shows program names.** For a command line, the report learns only which of
  the 75 listed programs it runs. Never arguments, paths, output, prompts or
  messages. A tool's name is read only when it looks like a name.
- **Some calls have no time.** When calls in a thread that started apart are
  all marked finished in the same instant, the provider closed them at once,
  most likely when a run stopped. They're counted, but their time isn't known,
  so it isn't added and they're not among the longest. A call that never ended
  adds no time either. The report says how many of each, and totals of time
  can only be too small.
- **A rate is never shown under 20 calls.** It says "too few to say."
- **Time is the sum of each call's own length.** Recap's tools hours are
  counted another way and won't match it.
- **It follows your theme,** and `--theme NAME` picks one. It needs 80 columns;
  a narrower terminal gets a plain list. At 120 columns, or with `--wide`, it
  prints a wider report: every row gains its typical and its longest call, and
  there are ten longest calls instead of five. `--compact` asks for the
  80-column one. Piped, or with `NO_COLOR` set, it is plain text.
- **`--this-week`** shows the current week so far, `--month` the last full
  month, `--this-month` the current one so far, and `--json` prints the
  numbers. When the last week or month has no records and the current one
  does, that is what you get anyway.
- **Times are local.** A week starts on Monday at midnight where you are.

## Trace

Press `w` on an agent in the dashboard and you get its task on a clock: when
you asked, each tool call, each time it waited on you, each agent it handed
work to, all on one time axis, live. It answers "why is this taking so long?"
while it's happening, and "where did the time go?" once it's done.
`t3-agents --trace` prints the latest task the same way, and
`t3-agents --trace --demo` shows a made-up one.

- **Where the time went,** split the way Recap splits a week: tools, the model,
  waiting on you, and the stretches when no run was open because a delegate
  was still working. The model's part says how much of it T3 actually saw
  (reasoning, writing a reply) and how much is just the time between steps.
- **One row per step that took real time,** in the order they happened, on a
  true clock. Most tool calls take under a second; they fold into one row that
  says how many there were and how long they took together. A step still
  running reaches "now" and its clock ticks.
- **Commands are named by their program,** from a fixed list (`cargo`, `git`,
  `rg`, `python3` and the like), looking past `bash -lc`, `cd … &&` and the
  rest of a chain. A command with nothing on the list is "other". The rest of
  the command line is never read out.
- **Delegates** sit on the same axis in their provider's color. `Enter` on one
  opens its own trace; one that ran on another machine says so.
- **The task** is the run you asked for plus the runs T3 itself marks as
  carrying on that work, such as the run that wakes when a delegate reports
  back. `←` `→` step through the thread's other tasks.

Good to know:

- **No progress bar, no estimate.** T3 doesn't know when a task will end, so
  Trace shows what has happened and what is in flight.
- **Long quiet stretches fold.** When nothing at all was happening in a task for
  more than half an hour (no run, no tool call, no question, no delegate), the
  axis folds that stretch to a narrow marker with its length on it, such as
  `┊14h┊`, so a task that waited overnight still shows its work. That time still
  counts as "no run open"; everywhere else the axis is true time.
- **Calls a provider closed all at once,** at one instant when the run ended,
  have no length of their own; they count as calls and add no time.
- **It costs little.** While open it rereads that one task every two seconds;
  closed, it costs nothing.

## The bar widget

- A dim dot cell, the dashboard's spinner at rest, when nothing's running.
  While agents work it spins on the dashboard's beat with their count before
  it; with animations off it holds one lit frame, so running never looks idle.
- A `◆` with a count in the bar's alert color when one needs you.
- An alert icon at full strength when it can't see T3: no dashboard installed
  yet, no T3 database, or a watcher that stopped. Its card says which, in
  plain words. It keeps retrying on its own, and it's never hidden, so a broken
  monitor never passes for a quiet one.
- Click for a card that lists them: the ones that need you first, in the alert
  color, then the ones running, each with its model, what it's doing and how
  much of its context it has used. `Esc` closes it; `Enter` or its button opens
  the dashboard.
- Right-click opens the dashboard straight away, middle-click restarts the
  watcher, and hovering gives a one-line summary.

![The bar widget's card on stock Omarchy, with demo data](bar-widget.png)
