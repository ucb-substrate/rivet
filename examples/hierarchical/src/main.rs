use cadence::innovus::{
    HardMacroConstraint, ObstructionConstraint, PlacementConstraints, TopLevelConstraint,
};
use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_cadence_reference_flow};
use std::path::PathBuf;

fn main() {
    let mut flow = sky130_cadence_reference_flow(
        PathBuf::from("/home/ff/eecs251b/"),
        PathBuf::from("/scratch/cs199-cbc/rivet/examples/hierarchical/src"),
        Dag {
            node: ModuleInfo {
                module_name: "nbitadder".into(),
                pin_info: FlatPinInfo::None,
                verilog_path: PathBuf::from(
                    "/scratch/cs199-cbc/rivet/examples/hierarchical/src/nbitadder.v",
                ),
                placement_constraints: PlacementConstraints {
                    top: TopLevelConstraint {
                        width: 1000.0,
                        height: 1000.0,
                        left: 0.0,
                        bottom: 0.0,
                        right: 0.0,
                        top: 0.0,
                    },
                    hard_macros: Some(vec![
                        HardMacroConstraint {
                            x: 10.0,
                            y: 10.0,
                            width: 100.0,
                            height: 100.0,
                            orientation: "r0".into(),
                            top_layer: "met3".into(),
                            stackup: vec![
                                "li1".into(),
                                "met1".into(),
                                "met2".into(),
                                "met3".into(),
                                "met4".into(),
                                "met5".into(),
                            ],
                            spacing: 2.0,
                            par_blockage_ratio: 1.2,
                            create_physical: false,
                            name: "adder_chain0".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 100.0,
                            y: 100.0,
                            width: 100.0,
                            height: 100.0,
                            orientation: "r0".into(),
                            top_layer: "met3".into(),
                            stackup: vec![
                                "li1".into(),
                                "met1".into(),
                                "met2".into(),
                                "met3".into(),
                                "met4".into(),
                                "met5".into(),
                            ],
                            spacing: 2.0,
                            par_blockage_ratio: 1.2,
                            create_physical: false,
                            name: "adder_chain1".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 300.0,
                            y: 100.0,
                            width: 100.0,
                            height: 100.0,
                            orientation: "r0".into(),
                            top_layer: "met3".into(),
                            stackup: vec![
                                "li1".into(),
                                "met1".into(),
                                "met2".into(),
                                "met3".into(),
                                "met4".into(),
                                "met5".into(),
                            ],
                            spacing: 2.0,
                            par_blockage_ratio: 1.2,
                            create_physical: false,
                            name: "adder_chain2".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 500.0,
                            y: 100.0,
                            width: 100.0,
                            height: 100.0,
                            orientation: "r0".into(),
                            top_layer: "met3".into(),
                            stackup: vec![
                                "li1".into(),
                                "met1".into(),
                                "met2".into(),
                                "met3".into(),
                                "met4".into(),
                                "met5".into(),
                            ],
                            spacing: 2.0,
                            par_blockage_ratio: 1.2,
                            create_physical: false,
                            name: "adder_chain3".into(),
                            master: "fulladder".into(),
                        },
                    ]),
                    obstructs: None,
                },
            },
            directed_edges: vec![Dag {
                node: ModuleInfo {
                    module_name: "fulladder".into(),
                    pin_info: FlatPinInfo::None,
                    verilog_path: PathBuf::from(
                        "/scratch/cs199-cbc/rivet/examples/hierarchical/src/fulladder.v",
                    ),
                    placement_constraints: PlacementConstraints {
                        top: TopLevelConstraint {
                            width: 100.0,
                            height: 100.0,
                            left: 0.0,
                            bottom: 0.0,
                            right: 0.0,
                            top: 0.0,
                        },
                        hard_macros: Some(vec![
                            HardMacroConstraint {
                                x: 10.0,
                                y: 10.0,
                                width: 30.0,
                                height: 30.0,
                                orientation: "r0".into(),
                                top_layer: "met3".into(),
                                stackup: vec![
                                    "li1".into(),
                                    "met1".into(),
                                    "met2".into(),
                                    "met3".into(),
                                    "met4".into(),
                                    "met5".into(),
                                ],
                                spacing: 2.0,
                                par_blockage_ratio: 1.2,
                                create_physical: false,
                                name: "ha1".into(),
                                master: "fulladder".into(),
                            },
                            HardMacroConstraint {
                                x: 50.0,
                                y: 10.0,
                                width: 30.0,
                                height: 30.0,
                                orientation: "r0".into(),
                                top_layer: "met3".into(),
                                stackup: vec![
                                    "li1".into(),
                                    "met1".into(),
                                    "met2".into(),
                                    "met3".into(),
                                    "met4".into(),
                                    "met5".into(),
                                ],
                                spacing: 2.0,
                                par_blockage_ratio: 1.2,
                                create_physical: false,
                                name: "ha2".into(),
                                master: "fulladder".into(),
                            },
                        ]),
                        obstructs: None,
                    },
                },

                directed_edges: vec![Dag {
                    node: ModuleInfo {
                        module_name: "halfadder".into(),
                        pin_info: FlatPinInfo::None,
                        verilog_path: PathBuf::from(
                            "/scratch/cs199-cbc/rivet/examples/hierarchical/src/halfadder.v",
                        ),
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
                }],
            }],
        },
    );

    flow.get_mut(&"nbitadder".to_string())
        .unwrap()
        .syn
        .replace_hook("syn_opt", "syn_opt", "syn_map", false);
    execute(flow.node.par);
}
