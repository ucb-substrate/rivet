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

Concurrency defaults to `RIVET_JOBS` if set, otherwise the core count. Tools
that hold licences or saturate a machine on their own are usually worth capping
explicitly.

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

Either way the rule is *stop starting, don't stop running*: steps already in
flight are allowed to finish, nothing new is dispatched, dependents of the
failed step never run, and the run ends with `ExecuteError::Failed` listing
every step that failed and the substep it died in.

```text
  ✖ decoder lvs      0.3s  during place_opt_design (3/5)  LVS mismatch: 3 unmatched nets
  ✔ decoder drc      0.9s
  ✖ 2 executed · 1 failed · 0.9s
```

A dependency cycle is reported as `ExecuteError::Cycle` rather than hanging.

While a flow runs, each executing step gets a line with a spinner, its elapsed
time, and the most recent line of output from the tool it is driving; finished
steps scroll off as `✔` (executed), `⏭` (pinned) or `✖` (failed). Full tool
output always goes to `{step}.out` and `{step}.err` in the step's work directory.
`ExecuteConfig::output(OutputMode::Stream)` shows every line instead of just
the tail. When stderr is not a terminal the display degrades to plain
one-line-per-event logging.

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

Once a step reports a substep with a position, its spinner gains a bar over the
substeps:

```text
  ⠹ decoder par     12s ━━━━━╸──── 3/5 place_opt_design · <last line of output>
  ⠸ decoder drc      4s route check 2 of 9
```

The marker is matched anywhere in a line, so tools that prefix output with a
severity or timestamp still work, and banner lines never show up as output.
`progress::banner_named` omits the counts for tools that do not know how many
substeps they will run, and `progress::substep(current, total, name)` reports
one directly from a Rust step that is not shelling out at all.

Run `cargo run -p rivet --example parallel` to see it against a mock flow.
