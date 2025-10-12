# Rivet

Rivet is a tool for flow management with a focus on simplicity and fine-grained checkpointing. Rivet also aims to provide clear APIs via Rust's type system.

Rivet core contains a minimal feature set for constructing and executing flows with dependency pinning. Additional features are implemented in PDK/tool plugins. Such features include:
- Parametric flows
- TCL templating
- Tool-specific checkpointing
