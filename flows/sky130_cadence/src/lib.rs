use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::fs::File;
use cadence::cadence::{MmmcConfig, MmmcCorner, Substep, mmmc, sdc};
use cadence::genus::{GenusStep, dont_avoid_lib_cells, set_default_options};
use cadence::innovus::{InnovusStep, Layer, PinAssignment, set_default_process};
use indoc::formatdoc;
use rivet::{Dag, Step, hierarchical};
use sky130::{setup_techlef, sky130_connect_nets};

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, BufWriter},
    sync::Arc,
};

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub struct ModuleInfo {
    pub module_name: String,
    pub pin_info: FlatPinInfo,
    pub verilog_path: PathBuf,
}

pub enum FlatPinInfo {
    None,
    PinSyn(PathBuf),
    PinPar(PathBuf),
}

pub struct Sky130FlatFlow {
    pub module: String,
    pub syn: Arc<GenusStep>,
    pub par: Arc<InnovusStep>,
}

pub fn sky130_syn(
    pdk_root: &PathBuf,
    work_dir: &PathBuf,
    module: &String,
    verilog_path: &PathBuf,
    dep_info: &[(&ModuleInfo, &Sky130FlatFlow)],
    pin_info: &FlatPinInfo,
) -> GenusStep {
    let syn_con =
        MmmcConfig {
            sdc_files: vec![work_dir.join("clock_pin_constraints.sdc")],

            corners: vec![
                MmmcCorner {
                    name: "ss_100C_1v60.setup".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib",
                    )],
                    temperature: dec!(100.0),
                },
                MmmcCorner {
                    name: "ff_n40C_1v95.hold".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib",
                    )],
                    temperature: dec!(-40.0),
                },
                MmmcCorner {
                    name: "tt_025C_1v80.extra".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib",
                    )],
                    temperature: dec!(25.0),
                },
            ],

            setup: vec!["ss_100C_1v60.setup".to_string()],

            hold: vec![
                "ff_n40C_1v95.hold".to_string(),
                "tt_025C_1v80.extra".to_string(),
            ],

            dynamic: "tt_025C_1v80.extra".to_string(),

            leakage: "tt_025C_1v80.extra".to_string(),
        };
    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        &work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );

    GenusStep::new(
        work_dir,
        module,
        vec![
            set_default_options(),
            dont_avoid_lib_cells("ICGX1"),
            GenusStep::read_design_files(
                work_dir,
                verilog_path,
                syn_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
            ),
            GenusStep::elaborate(module),
            GenusStep::init_design(module),
            GenusStep::power_intent(work_dir),
            GenusStep::syn_generic(),
            GenusStep::syn_map(),
            GenusStep::add_tieoffs(),
            GenusStep::write_design(module),
        ],
        false,
        None,
        vec![],
    )
}

pub fn sky130_par(
    pdk_root: &PathBuf,
    work_dir: &PathBuf,
    module: &String,
    netlist: &PathBuf,
    dep_info: &[(&ModuleInfo, &Sky130FlatFlow)],
    pin_info: &FlatPinInfo,
    syn_step: Arc<GenusStep>,
) -> InnovusStep {
    let filler_cells = vec![
        "FILL0".into(),
        "FILL1".into(),
        "FILL4".into(),
        "FILL9".into(),
        "FILL16".into(),
        "FILL25".into(),
        "FILL36".into(),
    ];

    let assignment = PinAssignment {
        pins: "*".into(),
        module: "decoder".into(),
        patterns: "-spread_type range".into(),
        layer: "-layer {met4}".into(),
        side: "-side bottom".into(),
        start: "-start {30 0}".into(),
        end: "-end {0 0}".into(),
        assign: "".into(),
        width: "".into(),
        depth: "".into(),
    };

    let layers = vec![
        Layer {
            top: "met1".into(),
            bot: "met1".into(),
            spacing: dec!(4.000),
            trim_antenna: false,
            add_stripes_command: r#"add_stripes -nets {VDD VSS} -layer met1 -direction horizontal -start_offset -.2 -width .4 -spacing 3.74 -set_to_set_distance 8.28 -start_from bottom -switch_layer_over_obs false -max_same_layer_jog_length 2 -pad_core_ring_top_layer_limit met5 -pad_core_ring_bottom_layer_limit met1 -block_ring_top_layer_limit met5 -block_ring_bottom_layer_limit met1 -use_wire_group 0 -snap_wire_center_to_grid none"#.to_string(),
        },

        Layer {
            top: "met4".to_string(),
            bot: "met1".to_string(),
            spacing: dec!(2.000),
            trim_antenna: true,
            add_stripes_command: r#"add_stripes -create_pins 0 -block_ring_bottom_layer_limit met4 -block_ring_top_layer_limit met1 -direction vertical -layer met4 -nets {VSS VDD} -pad_core_ring_bottom_layer_limit met1 -set_to_set_distance 75.90 -spacing 3.66 -switch_layer_over_obs 0 -width 1.86 -area [get_db designs .core_bbox] -start [expr [lindex [lindex [get_db designs .core_bbox] 0] 0] + 7.35]"#.to_string(),
        },
        Layer {
            top: "met5".to_string(),
            bot: "met4".to_string(),
            spacing: dec!(2.000),
            trim_antenna: true,
            add_stripes_command: r#"add_stripes -create_pins 1 -block_ring_bottom_layer_limit met5 -block_ring_top_layer_limit met4 -direction horizontal -layer met5 -nets {VSS VDD} -pad_core_ring_bottom_layer_limit met4 -set_to_set_distance 225.40 -spacing 17.68 -switch_layer_over_obs 0 -width 1.64 -area [get_db designs .core_bbox] -start [expr [lindex [lindex [get_db designs .core_bbox] 0] 1] + 5.62]"#.to_string(),
        }
    ];

    let par_con =
        MmmcConfig {
            sdc_files: vec![
                work_dir.clone().join("clock_pin_constraints.sdc"),
                work_dir
                    .parent()
                    .unwrap()
                    .join(format!("syn-rundir/{}.mapped.sdc", module)),
            ],
            corners: vec![
                MmmcCorner {
                    name: "ss_100C_1v60.setup".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib",
                    )],
                    temperature: dec!(100.0),
                },
                MmmcCorner {
                    name: "ff_n40C_1v95.hold".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib",
                    )],
                    temperature: dec!(-40.0),
                },
                MmmcCorner {
                    name: "tt_025C_1v80.extra".to_string(),
                    libs: vec![pdk_root.join(
                        "sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib",
                    )],
                    temperature: dec!(25.0),
                },
            ],

            setup: vec!["ss_100C_1v60.setup".to_string()],

            hold: vec![
                "ff_n40C_1v95.hold".to_string(),
                "tt_025C_1v80.extra".to_string(),
            ],

            dynamic: "tt_025C_1v80.extra".to_string(),

            leakage: "tt_025C_1v80.extra".to_string(),
        };

    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        &work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );

    InnovusStep::new(
        work_dir,
        module,
        vec![
            set_default_process(130),
            InnovusStep::read_design_files(
                work_dir,
                module,
                &netlist,
                par_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
            ),
            InnovusStep::init_design(),
            InnovusStep::innovus_settings(),
            sky130_innovus_settings(),
            InnovusStep::floorplan_design(work_dir),
            sky130_connect_nets(),
            InnovusStep::power_straps(layers),
            InnovusStep::place_pins("5", "1", vec![assignment]),
            InnovusStep::place_opt_design(),
            InnovusStep::add_fillers(filler_cells),
            InnovusStep::route_design(),
            InnovusStep::opt_design(),
            InnovusStep::write_regs(),
            sky130_connect_nets(),
            InnovusStep::write_design(work_dir, module),
        ],
        false,
        None,
        vec![syn_step],
    )
}

pub fn sky130_innovus_settings() -> Substep {
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            ln -sfn pre_sky130_innovus_settings latest
            ##########################################################
            # Placement attributes  [get_db -category place]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db place_global_place_io_pins  true

            set_db opt_honor_fences true
            set_db place_detail_dpt_flow true
            set_db place_detail_color_aware_legal true
            set_db place_global_solver_effort high
            set_db place_detail_check_cut_spacing true
            set_db place_global_cong_effort high
            set_db add_fillers_with_drc false

            ##########################################################
            # Optimization attributes  [get_db -category opt]
            ##########################################################
            #-------------------------------------------------------------------------------

            set_db opt_fix_fanout_load true
            set_db opt_clock_gate_aware false
            set_db opt_area_recovery true
            set_db opt_post_route_area_reclaim setup_aware
            set_db opt_fix_hold_verbose true

            ##########################################################
            # Clock attributes  [get_db -category cts]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db cts_target_skew 0.03
            set_db cts_max_fanout 10
            #set_db cts_target_max_transition_time .3
            set_db opt_setup_target_slack 0.10
            set_db opt_hold_target_slack 0.10

            ##########################################################
            # Routing attributes  [get_db -category route]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db route_design_antenna_diode_insertion 1
            set_db route_design_antenna_cell_name "ANTENNA"

            set_db route_design_high_freq_search_repair true
            set_db route_design_detail_post_route_spread_wire true
            set_db route_design_with_si_driven true
            set_db route_design_with_timing_driven true
            set_db route_design_concurrent_minimize_via_count_effort high
            set_db opt_consider_routing_congestion true
            set_db route_design_detail_use_multi_cut_via_effort medium


            # For top module: snap die to manufacturing grid, not placement grid
            set_db floorplan_snap_die_grid manufacturing


            # note this is required for sky130_fd_sc_hd, the design has a ton of drcs if bottom layer is 1
                            # TODO: why is setting routing_layer not enough?
            set_db design_bottom_routing_layer 2
            set_db design_top_routing_layer 6
            # deprected syntax, but this used to always work
            set_db route_design_bottom_routing_layer 2
            "#
        ),
        name: "sky130_innovus_settings".into(),
    }
}

fn sky130_flat_flow(
    pdk_root: &PathBuf,
    work_dir: &PathBuf,
    module: &ModuleInfo,
    dep_info: &[(&ModuleInfo, &Sky130FlatFlow)],
) -> Sky130FlatFlow {
    let syn_work_dir = work_dir.join("syn-rundir");
    let syn = sky130_syn(
        pdk_root,
        &syn_work_dir,
        &module.module_name,
        &module.verilog_path,
        dep_info,
        &module.pin_info,
    );
    let syn_pointer = Arc::new(syn);
    let par_work_dir = work_dir.join("par-rundir");
    let output_netlist_path = syn_work_dir.join(format!("{}.mapped.v", module.module_name));
    let par = sky130_par(
        pdk_root,
        &par_work_dir,
        &module.module_name,
        &output_netlist_path,
        dep_info,
        &module.pin_info,
        Arc::clone(&syn_pointer),
    );
    Sky130FlatFlow {
        module: module.module_name.to_string(),
        syn: syn_pointer,
        par: Arc::new(par),
    }
}

pub fn sky130_reference_flow(
    pdk_root: PathBuf,
    work_dir: PathBuf,
    hierarchy: Dag<ModuleInfo>,
) -> Dag<Sky130FlatFlow> {
    // `hierarchical` is a helper function defined in rivet.
    hierarchical(&hierarchy, &|block: &ModuleInfo,
                               sub_blocks: Vec<(
        &ModuleInfo,
        &Sky130FlatFlow,
    )>|
     -> Sky130FlatFlow {
        sky130_flat_flow(
            &pdk_root,
            &work_dir.join(format!("build-{}", &block.module_name)),
            block,
            &sub_blocks,
        )
    })
}
