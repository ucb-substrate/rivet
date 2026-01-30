use cadence::innovus::{Floorplan, HardMacroConstraint, TopLevelConstraint};
use rivet::{Dag, execute};
use sky130_cadence::{FlatPinInfo, ModuleInfo, sky130_cadence_reference_flow};
use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let mut flow = sky130_cadence_reference_flow(
        PathBuf::from(std::env::var("SKY130PDK_OS_INSTALL_PATH")?),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/"),
        Dag {
            node: ModuleInfo {
                module_name: "fourbitadder".into(),
                pin_info: FlatPinInfo::None,
                verilog_paths: vec![
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fourbitadder.v"),
                ],
                placement_constraints: Floorplan {
                    top: TopLevelConstraint {
                        width: 300.0,
                        height: 300.0,
                        left: 0.0,
                        bottom: 0.0,
                        right: 0.0,
                        top: 0.0,
                    },
                    hard_macros: vec![
                        HardMacroConstraint {
                            x: 10.0,
                            y: 10.0,
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
                            route_halo_size: 2.0,
                            place_halo_size: 1.2,
                            create_physical: false,
                            name: "fa_1".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 10.0,
                            y: 150.0,
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
                            route_halo_size: 2.0,
                            place_halo_size: 1.2,
                            create_physical: false,
                            name: "fa_2".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 150.0,
                            y: 10.0,
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
                            route_halo_size: 2.0,
                            place_halo_size: 1.2,
                            create_physical: false,
                            name: "fa_3".into(),
                            master: "fulladder".into(),
                        },
                        HardMacroConstraint {
                            x: 150.0,
                            y: 150.0,
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
                            route_halo_size: 2.0,
                            place_halo_size: 1.2,
                            create_physical: false,
                            name: "fa_4".into(),
                            master: "fulladder".into(),
                        },
                    ],
                    obstructs: vec![],
                },
            },
            directed_edges: vec![Dag {
                node: ModuleInfo {
                    module_name: "fulladder".into(),
                    pin_info: FlatPinInfo::None,
                    verilog_paths: vec![
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fulladder.v"),
                    ],
                    placement_constraints: Floorplan {
                        top: TopLevelConstraint {
                            width: 100.0,
                            height: 100.0,
                            left: 0.0,
                            bottom: 0.0,
                            right: 0.0,
                            top: 0.0,
                        },
                        hard_macros: vec![
                            HardMacroConstraint {
                                x: 10.0,
                                y: 10.0,
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
                                route_halo_size: 2.0,
                                place_halo_size: 1.2,
                                create_physical: false,
                                name: "ha1".into(),
                                master: "fulladder".into(),
                            },
                            HardMacroConstraint {
                                x: 50.0,
                                y: 10.0,
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
                                route_halo_size: 2.0,
                                place_halo_size: 1.2,
                                create_physical: false,
                                name: "ha2".into(),
                                master: "fulladder".into(),
                            },
                        ],
                        obstructs: vec![],
                    },
                },

                directed_edges: vec![Dag {
                    node: ModuleInfo {
                        module_name: "halfadder".into(),
                        pin_info: FlatPinInfo::None,
                        verilog_paths: vec![
                            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/halfadder.v"),
                        ],
                        placement_constraints: Floorplan {
                            top: TopLevelConstraint {
                                width: 30.0,
                                height: 30.0,
                                left: 0.0,
                                bottom: 0.0,
                                right: 0.0,
                                top: 0.0,
                            },
                            hard_macros: vec![],
                            obstructs: vec![],
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
        .get()
        .replace_hook("syn_opt", "syn_opt", "syn_map", false);

    execute(flow.node.par);
    Ok(())
}
