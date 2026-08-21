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
`signoff` waits for both. Steps are identified by the address of their `Arc`, so
a step reached by several paths runs exactly once.

```rust
rivet::execute(signoff);                          // panics if a step fails

rivet::ExecuteConfig::new()                       // or handle failures yourself
    .concurrency(2)
    .run(signoff)?;
```

Concurrency defaults to `RIVET_JOBS` if set, otherwise the core count. Tools
that hold licences or saturate a machine on their own are usually worth capping
explicitly.

A pinned step is treated as up to date: it is skipped, and its dependencies are
neither walked nor run. If a step panics, steps already in flight are allowed to
finish, nothing new is started, and the run reports which steps failed. A
dependency cycle is reported rather than hanging.

While a flow runs, each executing step gets a line with a spinner, its elapsed
time, and the most recent line of output from the tool it is driving; finished
steps scroll off as `✔` (executed), `⏭` (pinned) or `✖` (failed). Full tool
output always goes to `{step}.out` and `{step}.err` in the step's work directory.
`ExecuteConfig::output(OutputMode::Stream)` shows every line instead of just
the tail. When stderr is not a terminal the display degrades to plain
one-line-per-event logging.

Run `cargo run -p rivet --example parallel` to see it against a mock flow.
