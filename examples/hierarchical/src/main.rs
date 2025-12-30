use cadence::innovus::{HardMacroConstraint, PlacementConstraints, TopLevelConstraint};
use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_cadence_reference_flow};
use std::path::PathBuf;

fn main() {
    let mut flow = sky130_cadence_reference_flow(
        PathBuf::from("/home/ff/eecs251b/"),
        PathBuf::from("/scratch/cs199-cbc/rivet/examples/hierarchical/src"),
        Dag {
            node: ModuleInfo {
                module_name: "fourbitadder".into(),
                pin_info: FlatPinInfo::PinSyn(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("src/build-fourbitadder/syn-rundir"),
                ),
                verilog_paths: vec![
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fourbitadder.v"),
                ],
                placement_constraints: PlacementConstraints {
                    top: TopLevelConstraint {
                        width: 300.0,
                        height: 300.0,
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
                            name: "fa_1".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 10.0,
                            y: 150.0,
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
                            name: "fa_2".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 150.0,
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
                            name: "fa_3".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 150.0,
                            y: 150.0,
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
                            name: "fa_4".into(),
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
                    verilog_paths: vec![
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fulladder.v"),
                    ],
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
                        verilog_paths: vec![
                            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/halfadder.v"),
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
                }],
            }],
        },
    );

    flow.get_mut(&"fourbitadder".to_string())
        .unwrap()
        .syn
        .replace_hook("syn_opt", "syn_opt", "syn_map", false);

    flow.get_mut(&"fourbitadder".to_string())
        .unwrap()
        .par
        .add_checkpoint(
            "sky130_innovus_settings".to_string(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("src/build-fourbitadder/par-rundir/post_sky130_innovus_settings"),
        );

    execute(flow.node.par);
}
