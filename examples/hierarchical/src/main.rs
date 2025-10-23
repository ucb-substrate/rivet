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
    // want some function to add hooks to each flat flow or do you want to insert tcl to sky130_syn
    // and sky130_par templated tcl
    // I think u want for a given Dag of sky130 flat flows you want to be able to add custom tcl to
    // each flat flow or all the flat flows in general for synthesis and par
    //
    // prob want some api like this in the sky130
    // flow.add_hook(location: par/syn, custom tcl)
    // or flow.node.syn.add_hook(custom tcl)
    // or flow.node.par.add_hook(custom tcl)
    // or add hook(par/syn, custom tcl) and applies this to all parsteps or synstep
    execute(flow.node.par);
}
