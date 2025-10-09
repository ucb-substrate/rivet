use clap::Parser;
use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_reference_flow};
use std::fs;
use std::path::PathBuf;
use toml;

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
struct CliArgs {
    /// The name of the final flow node to execute (e.g., 'par').
    node: String,
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Path to the Rivet TOML configuration file.
    #[arg(long)]
    config: PathBuf,
}

fn main() {
    let flow = sky130_reference_flow(
        PathBuf::from("/home/ff/eecs251b/"),
        PathBuf::from("/scratch/cs199-cbc/rivet/examples/decoder/src"),
        Dag {
            node: ModuleInfo {
                module_name: "decoder".into(),
                pin_info: FlatPinInfo::None,
                verilog_path: PathBuf::from(
                    "/scratch/cs199-cbc/rivet/examples/decoder/src/decoder.v",
                ),
            },
            directed_edges: vec![],
        },
    );
    execute(flow.node.par);
}
