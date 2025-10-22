use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_cadence_reference_flow};
use std::path::PathBuf;

fn main() {
    let flow = sky130_cadence_reference_flow(
        PathBuf::from("/home/ff/eecs251b/"),
        PathBuf::from("/scratch/cs199-cbc/rivet/examples/hierarchical/src"),
        Dag {
            node: ModuleInfo {
                module_name: "nbitadder".into(),
                pin_info: FlatPinInfo::None,
                verilog_path: PathBuf::from(
                    "/scratch/cs199-cbc/rivet/examples/hierarchical/src/nbitadder.v",
                ),
            },
            directed_edges: vec![Dag {
                node: ModuleInfo {
                    module_name: "fulladder".into(),
                    pin_info: FlatPinInfo::None,
                    verilog_path: PathBuf::from(
                        "/scratch/cs199-cbc/rivet/examples/hierarchical/src/fulladder.v",
                    ),
                },
                directed_edges: vec![Dag {
                    node: ModuleInfo {
                        module_name: "halfadder".into(),
                        pin_info: FlatPinInfo::None,
                        verilog_path: PathBuf::from(
                            "/scratch/cs199-cbc/rivet/examples/hierarchical/src/halfadder.v",
                        ),
                    },
                    directed_edges: vec![],
                }],
            }],
        },
    );
    execute(flow.node.par);
}
