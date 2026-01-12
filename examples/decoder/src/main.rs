use cadence::innovus::{PlacementConstraints, TopLevelConstraint};
use clap::Parser;
use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_cadence_reference_flow};
use std::path::PathBuf;
use std::sync::Arc;

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
    let flow = sky130_cadence_reference_flow(
        PathBuf::from(env!("SKY130PDK_OS_INSTALL_PATH")),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/"),
        Dag {
            node: ModuleInfo {
                module_name: "decoder".into(),
                pin_info: FlatPinInfo::None,
                verilog_paths: vec![
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/decoder.v"),
                ],
                placement_constraints: PlacementConstraints {
                    top: TopLevelConstraint {
                        width: 30.0,
                        height: 30.0,
                        left: 0.0,
                        bottom: 0.0,
                        right: 0.0,
                        top: 0.0,
                    },
                    hard_macros: None,
                    obstructs: None,
                },
            },
            directed_edges: vec![],
        },
    );
    execute(flow.node.par);
}
