use cadence::genus::{
    GenusStep, add_tieoffs, dont_avoid_lib_cells, elaborate, power_intent, set_default_options,
    syn_generic, syn_init_design, syn_map, syn_read_design_files, syn_write_design,
};
use cadence::innovus::{
    InnovusStep, Layer, PinAssignment, PlacementConstraints, add_fillers, floorplan_design,
    innovus_settings, opt_design, par_init_design, par_read_design_files, par_write_design,
    place_opt_design, place_pins, place_tap_cells, power_straps, route_design, set_default_process,
    write_ilm, write_regs,
};
use cadence::{MmmcConfig, MmmcCorner, SubmoduleInfo, Substep, sdc};
use indoc::formatdoc;
use rivet::{Dag, NamedNode, Step, hierarchical};
use sky130::{setup_techlef, sky130_connect_nets};
use std::fs;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub struct ModuleInfo {
    pub module_name: String,
    pub pin_info: FlatPinInfo,
    pub verilog_path: PathBuf,
    pub placement_constraints: Vec<PlacementConstraints>,
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

impl NamedNode for Sky130FlatFlow {
    fn name(&self) -> String {
        self.module.clone()
    }
}

pub fn sky130_syn(
    pdk_root: &Path,
    work_dir: &PathBuf,
    module: &String,
    verilog_path: &Path,
    dep_info: &[(&ModuleInfo, &Sky130FlatFlow)],
    pin_info: &FlatPinInfo,
) -> GenusStep {
    let ss_100C_1v60 = MmmcCorner {
        name: "ss_100C_1v60".to_string(),
        corner_type: "setup".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib"),
        ],
        temperature: dec!(100.0),
    };
    let ff_n40C_1v95 = MmmcCorner {
        name: "ff_n40C_1v95".to_string(),
        corner_type: "hold".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib"),
        ],
        temperature: dec!(-40.0),
    };

    let tt_025C_1v80 = MmmcCorner {
        name: "tt_025C_1v80".to_string(),
        corner_type: "extra".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib"),
        ],
        temperature: dec!(25.0),
    };

    let syn_con = MmmcConfig {
        sdc_files: vec![work_dir.join("clock_pin_constraints.sdc")],

        corners: vec![
            ss_100C_1v60.clone(),
            ff_n40C_1v95.clone(),
            tt_025C_1v80.clone(),
        ],

        setup: vec![ss_100C_1v60.clone()],

        hold: vec![ff_n40C_1v95.clone(), tt_025C_1v80.clone()],

        dynamic: tt_025C_1v80.clone(),

        leakage: tt_025C_1v80.clone(),
    };
    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );

    let submodules: Vec<SubmoduleInfo> = dep_info
        .iter()
        .map(|(module, flow)| SubmoduleInfo {
            name: module.module_name.clone(),
            verilog: module.verilog_path.clone(),
            ilm: flow.par.ilm_path().to_path_buf(),
            lef: flow.par.lef_path().to_path_buf(),
        })
        .collect();

    let deps: Vec<Arc<dyn Step>> = dep_info
        .iter()
        .map(|(_module, flow)| Arc::clone(&flow.par) as Arc<dyn Step>)
        .collect();

    GenusStep::new(
        work_dir,
        module,
        vec![
            set_default_options(),
            dont_avoid_lib_cells("ICGX1"),
            syn_read_design_files(
                work_dir,
                verilog_path,
                syn_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
                Some(submodules.clone()),
            ),
            elaborate(module),
            syn_init_design(module, Some(submodules.clone())),
            power_intent(work_dir, &sky130_cadence_power_spec(module, dec!(1.8))),
            syn_generic(),
            syn_map(),
            add_tieoffs(),
            syn_write_design(module, ss_100C_1v60.clone()),
        ],
        matches!(pin_info, FlatPinInfo::PinSyn(_)),
        deps,
    )
}

pub fn sky130_par(
    pdk_root: &Path,
    work_dir: &PathBuf,
    module: &String,
    constraints: &Vec<PlacementConstraints>,
    netlist: &Path,
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
        module: module.into(),
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

    let ss_100C_1v60 = MmmcCorner {
        name: "ss_100C_1v60".to_string(),
        corner_type: "setup".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib"),
        ],
        temperature: dec!(100.0),
    };
    let ff_n40C_1v95 = MmmcCorner {
        name: "ff_n40C_1v95".to_string(),
        corner_type: "hold".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib"),
        ],
        temperature: dec!(-40.0),
    };

    let tt_025C_1v80 = MmmcCorner {
        name: "tt_025C_1v80".to_string(),
        corner_type: "extra".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib"),
        ],
        temperature: dec!(25.0),
    };

    let par_con = MmmcConfig {
        sdc_files: vec![work_dir.join("clock_pin_constraints.sdc")],

        corners: vec![
            ss_100C_1v60.clone(),
            ff_n40C_1v95.clone(),
            tt_025C_1v80.clone(),
        ],

        setup: vec![ss_100C_1v60.clone()],

        hold: vec![ff_n40C_1v95.clone(), tt_025C_1v80.clone()],

        dynamic: tt_025C_1v80.clone(),

        leakage: tt_025C_1v80.clone(),
    };

    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );

    let submodules: Vec<SubmoduleInfo> = dep_info
        .iter()
        .map(|(module, flow)| SubmoduleInfo {
            name: module.module_name.clone(),
            verilog: module.verilog_path.clone(),
            ilm: flow.par.ilm_path().to_path_buf(),
            lef: flow.par.lef_path().to_path_buf(),
        })
        .collect();

    let par_constraints = constraints.clone();

    InnovusStep::new(
        work_dir,
        module,
        vec![
            set_default_process(130),
            par_read_design_files(
                work_dir,
                module,
                netlist,
                par_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
                Some(submodules),
            ),
            par_init_design(),
            innovus_settings(2, 6),
            sky130_innovus_settings(),
            floorplan_design(
                work_dir,
                &sky130_cadence_power_spec(module, dec!(1.8)),
                par_constraints,
            ),
            sky130_connect_nets(),
            power_straps(layers.clone()),
            place_pins("5", "1", vec![assignment]),
            place_opt_design(None),
            add_fillers(filler_cells),
            route_design(),
            opt_design(),
            write_regs(),
            sky130_connect_nets(),
            par_write_design(
                pdk_root,
                work_dir,
                module,
                vec![
                    ss_100C_1v60.clone(),
                    ff_n40C_1v95.clone(),
                    tt_025C_1v80.clone(),
                ],
            ),
            write_ilm(
                work_dir,
                module,
                &layers[0],
                vec![
                    ss_100C_1v60.clone(),
                    ff_n40C_1v95.clone(),
                    tt_025C_1v80.clone(),
                ],
            ),
        ],
        matches!(pin_info, FlatPinInfo::PinPar(_)),
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

pub fn sky130_cadence_power_spec(module: &str, voltage: Decimal) -> String {
    formatdoc! {
    r#"
    set_cpf_version 1.0e
    set_hierarchy_separator /
    set_design {}
    create_power_nets -nets VDD -voltage {voltage}
    create_power_nets -nets VPWR -voltage {voltage} 
    create_power_nets -nets VPB -voltage {voltage}
    create_power_nets -nets vdd -voltage {voltage}
    create_ground_nets -nets {{ VSS VGND VNB vss }}
    create_power_domain -name AO -default
    update_power_domain -name AO -primary_power_net VDD -primary_ground_net VSS
    create_global_connection -domain AO -net VDD -pins [list VDD]
    create_global_connection -domain AO -net VPWR -pins [list VPWR]
    create_global_connection -domain AO -net VPB -pins [list VPB]
    create_global_connection -domain AO -net vdd -pins [list vdd]
    create_global_connection -domain AO -net VSS -pins [list VSS]
    create_global_connection -domain AO -net VGND -pins [list VGND]
    create_global_connection -domain AO -net VNB -pins [list VNB]
    create_nominal_condition -name nominal -voltage {voltage}
    create_power_mode -name aon -default -domain_conditions {{AO@nominal}}
    end_design
    "#, module.to_string()
    }
}

fn sky130_cadence_flat_flow(
    pdk_root: &Path,
    work_dir: &Path,
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

    //create the vec of vec of placement constraints with the current one being the top and the rest being
    //hierarchical
    let mut top_constraints = module.placement_constraints.clone();
    top_constraints[0].constraint_type = "TopLevel".into();
    let hierarchical_constraints: Vec<PlacementConstraints> = dep_info
        .iter()
        .flat_map(|(module, flow)| module.placement_constraints.clone())
        .collect();

    let mut final_constraints = top_constraints.clone();
    final_constraints.extend(hierarchical_constraints);
    let par = sky130_par(
        pdk_root,
        &par_work_dir,
        &module.module_name,
        &final_constraints,
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

pub fn sky130_cadence_reference_flow(
    pdk_root: PathBuf,
    work_dir: PathBuf,
    hierarchy: Dag<ModuleInfo>,
) -> Dag<Sky130FlatFlow> {
    hierarchical(&hierarchy, &|block: &ModuleInfo,
                               sub_blocks: Vec<(
        &ModuleInfo,
        &Sky130FlatFlow,
    )>|
     -> Sky130FlatFlow {
        sky130_cadence_flat_flow(
            &pdk_root,
            &work_dir.join(format!("build-{}", &block.module_name)),
            block,
            &sub_blocks,
        )
    })
}
