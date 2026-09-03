# Rivet

Rivet is a tool for flow management with a focus on simplicity and fine-grained checkpointing. Rivet also aims to provide clear APIs via Rust's type system.

Rivet core contains a minimal feature set for constructing and executing flows with dependency pinning. Additional features are implemented in PDK/tool plugins. Such features include:
- Parametric flows
- TCL templating
- Tool-specific checkpointing

For the sake of simplicity, Rivet does **not** include features that other flow managers may provide, such as:
- Intermediate representations for portability between tools and technologies
- Automatic caching

## Execution

`rivet::execute` runs a target step and everything it depends on. The graph is
walked once up front, then executed by a pool of worker threads: a step starts
as soon as all of its dependencies have finished, so independent branches run
concurrently.

```text
  syn ──▶ par ──┬──▶ drc ──┐
                └──▶ lvs ──┴──▶ signoff
```

Here `drc` and `lvs` both wait for `par`, then run at the same time, and
`signoff` waits for both. Steps are identified by the address of their
`StepRef`, so a step reached by several paths runs exactly once.

```rust
rivet::execute(signoff);                          // panics if a step fails

rivet::ExecuteConfig::new()                       // or handle failures yourself
    .concurrency(2)
    .run(signoff)?;
```

Several targets can be queued on an `Executor`. They are flattened into one
graph, so work shared between them still happens once and independent branches
of either still overlap:

```rust
rivet::Executor::new()
    .concurrency(2)
    .target(drc)
    .target(lvs)
    .run()?;
```

Concurrency defaults to the core count and is set in code, not by the
environment. Tools that hold licences or saturate a machine on their own are
usually worth capping explicitly.

A pinned step is treated as up to date: it is skipped, and its dependencies are
neither walked nor run.

### Failure

`Step::execute` returns `StepResult`. A step reports an expected failure — a
tool exiting non-zero, LVS not matching, a missing input — by returning `Err`;
`?` converts any error type, and a message becomes one with `.into()`:

```rust
fn execute(&self) -> StepResult {
    let status = exec::run_logged_in(&mut command, &self.work_dir, "lvs")?;
    if !status.success() {
        return Err(format!("LVS did not match for {}", self.module).into());
    }
    Ok(())
}
```

Panicking is for bugs. The executor catches panics so one cannot take down the
run, but reports them separately (`StepFailure::panicked`) because a panic means
something is wrong with the step itself rather than with the design.

Either way the rule is *drop the branch, not the run*: a failure takes down the
steps that depended on it and nothing else. Steps already in flight are left to
finish, every other branch keeps going, and steps that have not started yet are
still dispatched as long as they do not depend on anything that failed — so a
run gets as far as it can before it stops. Dependents of a failed step can never
become runnable, so they are dropped as `⊘` and named, transitively.

The run ends with `ExecuteError::Failed`, listing every step that failed and
where it was when it failed, plus the steps that never ran as a result:

```text
  ✔ decoder merge    0.9s
  ✖ decoder lvs      1.2s  during compare (2/2)  LVS mismatch: 3 unmatched nets
  ⊘ decoder signoff  blocked by decoder lvs
  ✔ decoder drc      2.8s
  ✖ 4 executed · 1 skipped · 1 blocked · 1 failed · 6.9s
```

`decoder drc` and `decoder merge` do not depend on LVS, so they run to
completion; `decoder signoff` does, so it is dropped rather than left to look
stalled.

"Where" is both halves of the step's line when both are set — which of them
caused the failure is exactly what is not known at that point:

```text
  ✖ decoder par  2m14s  during merging gds (7/12) │ add_fillers (5/5)  innovus exited with 1
```

A tool that exits cleanly has its substep cleared, so a step that then fails in
its own post-processing is not blamed on a substep that finished fine. A tool
that exits non-zero keeps it, because that is the substep you want named.

A dependency cycle is reported as `ExecuteError::Cycle` rather than hanging.

While a flow runs, every step gets a line: a spinner, its elapsed time and
whatever progress it reports (see below) while it runs, and afterwards how it
ended — `✔` (executed), `⏭` (pinned), `⊘` (blocked by a failure) or `✖`
(failed). The lines are a list on a screen of their own, the terminal's
alternate screen, the way an editor takes it: the list holds every step in the
run and scrolls under the cursor once there are more than fit, with the run's
summary and the key hints at the bottom. The screen stays up
when the run is over, so a finished run can be looked through — every step
under the cursor, a failure's log a keypress away — instead of turning into
terminal history the moment it ends. `q` gives the terminal back, and leaves
the run's record in the ordinary scrollback: one line per step, in the order
things happened.

Raw tool output is never drawn on a step's line: it goes to `{step}.out` and
`{step}.err` in the step's work directory, and the step's own page reads those
files back (see below). Two things reach the line, and nothing else — a step's
`status`, set from Rust, and the substep banners a tool is told to print. Which
stream a tool chose means nothing; plenty put all their chatter on stderr. When
stderr is not a terminal the display degrades to plain one-line-per-event
logging.

### Substep banners

A step such as P&R is one node to the scheduler but a long sequence of substeps
to the tool driving it. A tool can say which substep it is on by printing a
marker line, which rivet picks out of the output stream:

```text
<<rivet:substep 3/5 place_opt_design>>
```

Build one with `progress::banner(current, total, name)`. `GenusStep`,
`InnovusStep` and `PegasusStep` emit one per substep into the TCL they generate:

```tcl
puts {<<rivet:substep 3/5 place_opt_design>>}
```

The marker is matched anywhere in a line, so tools that prefix output with a
severity or timestamp still work, and banner lines never show up as output.
`progress::banner_named` omits the counts for tools that do not know how many
substeps they will run.

Banners are the only way to fill this half of the line: it is reached by parsing
the tool's output and nothing else, so what it shows always reflects what the
tool actually said. Progress the Rust side knows about goes in the status
instead.

### The step's line

A running step has two independent halves, either of which can carry its own
bar:

```text
  ⠹ decoder par  12s ━━╸─────── 3/12 merging gds │ ━━━╸────── 2/5 route_design
                     └──────── status ────────┘   └──────── banner ────────┘
```

The **left** half is the step's own status, and the only half Rust writes. Set
it with `progress::status(msg)` or `progress::status_progress(current, total,
msg)` — useful for work a step does itself, where there is no tool output to
parse:

```rust
for (index, file) in gds_files.iter().enumerate() {
    progress::status_progress(index + 1, gds_files.len(), format!("merging {file}"));
    merge(file)?;
}
```

The **right** half is the substep banner picked out of the tool's output, and
only ever comes from there.

The two never interfere: a banner cannot clear the status, and a status cannot
clear the banner. Each half is omitted entirely until something fills it.

### Reading a step's log

Every step in the run has a line from the start, and is under a cursor, moved
between them with `↑`/`↓` or `j`/`k` (and `PgUp`/`PgDn`, `g`/`G` for the ends).
Over the list, a banner says what the run is: its targets, how many steps and
workers it has, and where its logs go — shrunk to a line on a short terminal,
and gone on a very short one:

```text
  █▀▄ █ █ █ █▀▀ ▀█▀   decoder signoff
  █▀▄ █ ▀▄▀ █▀▀  █    7 steps · 4 workers
  ▀ ▀ ▀  ▀  ▀▀▀  ▀    logs in build

  ⏭ sram compile     pinned
  ✔ decoder syn      1m14s
  ✖ decoder lvs      2m01s  during compare (2/2)  lvs did not match; see build/decoder.lvs.out
  ⊘ decoder signoff  blocked by decoder lvs
  ⠹ decoder drc      1m02s ━━━╸────── 1/3 density
❯ ⠹ decoder par     12m08s ━━╸─────── 3/12 merging gds │ ━━━╸────── 2/5 route_design
  ○ decoder merge    waits for decoder par

  ━━━━━━╸───────────────── 4/7 steps · 12m08s · 2 running · 1 blocked · 1 failed
  ↑/↓ or j/k move · enter open a step · y copy a less command · q cancel the run
```

The list is in four groups: the pinned steps, which are over before the run
begins; the steps that have finished, in the order they finished; the steps
running now, in the order they started; and, greyed, the steps still to come,
in the order the run is expected to take them, each naming the steps it is
still waiting for. A step moves up from group to group as the run goes — from
waiting to running when it starts, and into the finished steps when it ends.

A finished step keeps its colour: it is as much there to be opened as a
running one. A failure carries the substep it died in and its message, and
that line wraps onto further rows rather than being cut at the edge of the
screen, so the whole of it can be read; a running step's line stays on one row,
so the list does not jump about as its status changes length.

The cursor stays on the step it is on. Steps starting never move it; the one
thing that does is the step under it finishing, when it goes to the newest step
still running — so someone watching the run keeps watching the run, and is not
left on what the step turned into. Put on a step that has already finished, it
stays there until you move it. With nothing else running when its step
finishes, it waits there and takes the next step to start.

`enter` opens the step under the cursor. Its page is the step's log as it is
written, with the step's own line — in full, wrapped — and the run's summary
underneath:

```text
 decoder par  build/decoder/par/decoder.par.out ────────────────────────────────────────────
<<rivet:substep 2/5 route_design>>
#% Begin route_design (date=09/02 14:12:08, mem=4.2G)
#Routing layer 4 of 6 ...
 ...
────────────────────────────────────────────── 1/3 files (tab)  following · 4,120 lines
  ⠹ decoder par     12m08s ━━╸─────── 3/12 merging gds │ ━━━╸────── 2/5 route_design
  ━━━━━━╸───────────────── 4/7 steps · 12m08s · 2 running · 1 blocked · 1 failed
  esc back · ↑/↓ scroll · G follow · tab next file · y copy a tail command · q quit
```

The page follows the end of the file as it grows, the way `tail -F` does, and
is scrolled back through with `↑`/`↓`, `PgUp`/`PgDn` and `g`; `G` goes back to
following. Long lines wrap by column, so the end of a long line is there to be
read. `tab` moves between the step's files: the output of the tool it is running
now, first — the `.out` and `.err` that `exec::run_logged` is writing — then the
output of tools it ran earlier, then the step's own `{step}.rivet.log`. A step
driving a tool some other way says what it is writing with
`StepHandle::set_output_files`. A pinned or blocked step, which did not run this
time, offers its `{step}.rivet.log` from the run that last ran it; a step that
has not started yet offers nothing until it does, since the log at its path is
the last run's and about to be replaced. `esc` comes back to the list, on the
step that was open.

`y` copies a `less` command for the same files to the clipboard, for a terminal
of your own — the whole log, since the tail of it is what the page already
shows:

```text
less /build/decoder/par/decoder.par.out /build/decoder/par/decoder.par.err
```

The copy is asked for with OSC 52, which is the terminal's own way of doing it
and therefore the one that works over ssh: the clipboard that matters is on the
machine the person is watching from, not the compute server the flow is running
on. A local `wl-copy`, `xclip`, `xsel` or `pbcopy` is asked as well, if the
environment suggests one would work.

On a terminal too narrow for a line, the line gives up what it can best spare
before anything is cut: the hint is said more tersely, the summary's bar is
squeezed before its counts are, a running step's line drops its bars (the
`3/12` beside each says as much) and is then cut with an ellipsis, the label
column is squeezed only if a running line still has no room, and a long path on
a step's page is cut from the left so its file name stays. A terminal wide
enough for everything draws everything, exactly as it would have.

Because the display owns the whole screen, it can be redrawn from nothing at
any time: resizing the terminal redraws it, and so does `^L`, for when
something has written over it — a stray `println!` in flow code lands on the
alternate screen, and is gone with it.

While the run is going, the display is where it is controlled from. `q` cancels
the run — after asking, since the answer kills every tool the run has going —
by sending the same interrupt `^C` would, to the whole process group; the run
ends as an interrupted one, exit code `130`, with its record so far left in the
terminal. `^C` itself cancels at once, without asking: it is a signal, not a
key, so it works whatever the display is doing. Once the run is over, `q`
simply quits. A run that is not wanted on screen at all is started with the
display turned off, `ExecuteConfig::progress(false)`, and reports plainly
instead.

While the display is up, the terminal hands over keys as they are typed rather
than collecting whole lines, and stops echoing them. The signal keys are put
back afterwards, so `^C` and `^Z` mean exactly what they always did — `^C` in
particular has to keep reaching the tools a step is running, which are in the
same process group, and has to keep working when the display itself is not
answering. An interrupt is caught only so that the terminal can be handed back
before the run ends, and the run exits `130`; a second one gives up on being
tidy and exits at once. `^Z` is caught the same way, so that the screen is put
away before the process stops and taken again when it is continued.

Only stderr has to be a terminal, since that is the stream the display draws
on; the keys and the terminal's size come from the controlling terminal itself.
A run whose stdout is redirected still gets the display, and anything flow code
prints to stdout lands in the file rather than over the screen. A run in CI, or
with stderr redirected, falls back to plain one-line-per-event logging on
stderr.


## Logging

The display owns stderr while a flow runs, so a log line printed to a stream
would corrupt it. Logging goes to files instead, through `tracing`:

```rust
tracing::info!(unmatched, "LVS did not match");
```

There is nothing to set up — the executor installs the subscriber — and an
event is written twice:

```text
build/
  rivet.log                  the whole run, every step, in order
  decoder/par/
    decoder.par.out          raw innovus stdout
    decoder.par.err          raw innovus stderr
    decoder par.rivet.log    what this step logged, and nothing else
```

`rivet.log` goes in `ExecuteConfig::log_dir` (the current directory by default)
and is appended to, with a blank line between runs. A step's own log goes
wherever `Step::log_dir` says the step lives, next to the output of the tools it
drove, and is rewritten each time the step runs, like the `.out` and `.err`
beside it. A step that returns `None` — the default — still reaches `rivet.log`;
it just has nowhere of its own to go.

Events are tagged with the step that emitted them, so `rivet.log` reads as one
narrative even with several steps in flight:

```text
18:02:11.401Z  INFO rivet::executor: step{name=decoder par}: started
18:02:11.402Z  INFO rivet::exec: step{name=decoder par}: running command="innovus" "-files" "par.tcl" stdout="…/decoder.par.out" stderr="…/decoder.par.err"
18:06:12.884Z  INFO rivet::exec: step{name=decoder par}: exited code=0 success=true
18:06:13.002Z  INFO rivet::executor: step{name=decoder par}: completed elapsed=4m1.6s
```

`RIVET_LOG` sets what is kept, in the usual `EnvFilter` syntax — `RIVET_LOG=debug`,
or `RIVET_LOG=rivet=info,cadence=debug` to turn one plugin up. It defaults to
`info`.

Tool output is not folded in: there is far too much of it for a log meant to
stay readable, and it is already captured in full next door. What rivet records
is which command a step ran, where that output went, and how it ended.

So there are three channels, and nothing writes to two of them:

| | goes to | written by |
|---|---|---|
| raw tool output | `{step}.out` / `.err` | the tool, captured by `exec` |
| the live display | stderr | `progress::status` and substep banners |
| the run's own record | `rivet.log`, `{step}.rivet.log` | `tracing` |

Run `cargo run -p rivet --example parallel` to see it against a mock flow.
