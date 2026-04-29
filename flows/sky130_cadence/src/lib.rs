use cadence::genus::{
    GenusStep, add_tieoffs, dont_avoid_lib_cells, elaborate, power_intent, set_default_options,
    syn_generic, syn_init_design, syn_map, syn_read_design_files, syn_write_design,
};
use cadence::innovus::{
    Floorplan, InnovusStep, Layer, PinAssignment, add_fillers, floorplan_design, innovus_settings,
    opt_design, par_init_design, par_read_design_files, place_opt_design,
    place_pins, power_straps, route_design, set_default_process, write_ilm, write_regs,
};
use cadence::{MmmcConfig, MmmcCorner, SubmoduleInfo, Substep};
use indoc::formatdoc;
use rivet::bash::BashStep;
use rivet::{Dag, NamedNode, Step, StepRef, hierarchical};
use sky130::{setup_techlef, sky130_connect_nets};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub struct ModuleInfo {
    pub module_name: String,
    pub pin_info: FlatPinInfo,
    pub verilog: Vec<PathBuf>,
    pub srams: Vec<Sram22>,
    pub placement_constraints: Floorplan,
    pub floorplan_commands: String,
    pub routing_top_layer: usize,
    pub power_top_layer: usize,
    pub sdc: String,
}

#[derive(Clone, Debug)]
pub struct Sram22 {
    pub num_words: u64,
    pub data_width: u64,
    pub mux_ratio: u64,
    pub write_size: u64,
}

impl Sram22 {
    pub fn name(&self) -> String {
        format!(
            "sram22_{}x{}m{}w{}",
            self.num_words, self.data_width, self.mux_ratio, self.write_size
        )
    }

    fn sram_dir(&self, sram_work_dir: &Path) -> PathBuf {
        sram_work_dir.join(self.name())
    }

    pub fn lef(&self, sram_work_dir: &Path) -> PathBuf {
        self.sram_dir(sram_work_dir).join(format!("{}.lef", self.name()))
    }

    pub fn verilog(&self, sram_work_dir: &Path) -> PathBuf {
        self.sram_dir(sram_work_dir).join(format!("{}.v", self.name()))
    }

    pub fn spice(&self, sram_work_dir: &Path) -> PathBuf {
        self.sram_dir(sram_work_dir).join(format!("{}.spice", self.name()))
    }

    pub fn gds(&self, sram_work_dir: &Path) -> PathBuf {
        self.sram_dir(sram_work_dir).join(format!("{}.gds", self.name()))
    }

    pub fn ensure_generated(&self, sram_work_dir: &Path) -> bool {
        self.lef(sram_work_dir).exists()
            && self.gds(sram_work_dir).exists()
            && self.spice(sram_work_dir).exists()
    }
}

/// Writes sram22.toml configs and a run_generate_sram.sh script into sram_work_dir.
/// sram22 is invoked from $SRAM22_ROOT with output directed to sram_work_dir/{sram_name}/.
pub fn generate_compiler_script(srams: &[Sram22], sram_work_dir: &Path) -> anyhow::Result<()> {
    let sram22_root =
        std::env::var("SRAM22_ROOT").expect("SRAM22_ROOT environment variable must be set");

    let script_path = sram_work_dir.join("run_generate_sram.sh");
    let mut script = fs::File::create(&script_path)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    writeln!(script, "#!/bin/bash")?;
    writeln!(script, "set -e")?;

    for sram in srams {
        let name = sram.name();
        let sram_output_dir = sram.sram_dir(sram_work_dir);
        let sram_output_dir_str = sram_output_dir.display();

        let config_path = sram_work_dir.join(format!("{}.toml", name));
        let config_content = format!(
            "num_words = {}\ndata_width = {}\nmux_ratio = {}\nwrite_size = {}\n",
            sram.num_words, sram.data_width, sram.mux_ratio, sram.write_size
        );
        fs::write(&config_path, config_content)?;

        let config_path_str = config_path.display();
        let lef_path = sram.lef(sram_work_dir).display().to_string();
        let gds_path = sram.gds(sram_work_dir).display().to_string();

        writeln!(script, "echo \"Generating SRAM: {name}\"")?;
        writeln!(script, "mkdir -p {sram_output_dir_str}")?;
        writeln!(script, "cd {sram22_root}")?;
        writeln!(script, "sram22 --config {config_path_str} --output {sram_output_dir_str}")?;
        writeln!(script)?;
        writeln!(
            script,
            "if [ ! -f \"{lef_path}\" ] || [ ! -f \"{gds_path}\" ]; then"
        )?;
        writeln!(
            script,
            "    echo \"ERROR: SRAM compiler failed for {name}\" >&2"
        )?;
        writeln!(script, "    exit 1")?;
        writeln!(script, "fi")?;
    }

    Ok(())
}

fn sky130_syn_read_design_files(
    work_dir: &Path,
    verilog_paths: &[PathBuf],
    mmmc_conf: MmmcConfig,
    tlef: &Path,
    pdk_lef: &Path,
    submodules: Option<Vec<SubmoduleInfo>>,
    is_hierarchical: bool,
    srams: &[Sram22],
) -> Substep {
    let mut substep = syn_read_design_files(
        work_dir,
        verilog_paths,
        mmmc_conf,
        tlef,
        pdk_lef,
        submodules,
        is_hierarchical,
    );

    if !srams.is_empty() {
        let sram_work_dir = work_dir.parent().unwrap().join("sram");
        let sram_lefs: Vec<String> = srams
            .iter()
            .map(|s| s.lef(&sram_work_dir).display().to_string())
            .collect();
        let sram_verilog: Vec<String> = srams
            .iter()
            .map(|s| s.verilog(&sram_work_dir).display().to_string())
            .collect();
        substep.command.push_str(&format!(
            "\nread_physical -lef {{ {} }}\nread_hdl -sv {{ {} }}",
            sram_lefs.join(" "),
            sram_verilog.join(" ")
        ));
    }

    substep
}

fn sky130_par_read_design_files(
    work_dir: &Path,
    module: &str,
    netlist_path: &Path,
    mmmc_conf: MmmcConfig,
    tlef: &Path,
    pdk_lef: &Path,
    submodules: Option<Vec<SubmoduleInfo>>,
    srams: &[Sram22],
) -> Substep {
    let mut substep = par_read_design_files(
        work_dir,
        module,
        netlist_path,
        mmmc_conf,
        tlef,
        pdk_lef,
        submodules,
    );

    if !srams.is_empty() {
        let sram_work_dir = work_dir.parent().unwrap().join("sram");
        let sram_lefs: Vec<String> = srams
            .iter()
            .map(|s| s.lef(&sram_work_dir).display().to_string())
            .collect();
        substep
            .command
            .push_str(&format!("\nread_physical -lef {{ {} }}", sram_lefs.join(" ")));
    }

    substep
}

fn sky130_par_write_design(
    pdk_root: &Path,
    work_dir: &Path,
    module: &str,
    srams: &[Sram22],
    corners: Vec<MmmcCorner>,
) -> Substep {
    let par_rundir = work_dir.display().to_string();
    let module = module.to_owned();
    let setup = corners
        .iter()
        .find(|p| p.corner_type == "setup")
        .unwrap()
        .name
        .clone();
    let hold = corners
        .iter()
        .find(|p| p.corner_type == "hold")
        .unwrap()
        .name
        .clone();
    let typical = corners
        .iter()
        .find(|p| p.corner_type == "extra")
        .unwrap()
        .name
        .clone();

    let sram_work_dir = work_dir.parent().unwrap().join("sram");
    let pdk_gds = pdk_root
        .join("sky130/sky130_cds/sky130_scl_9T_0.0.5/gds/sky130_scl_9T.gds")
        .display()
        .to_string();
    let mut merge_gds = vec![pdk_gds];
    merge_gds.extend(
        srams
            .iter()
            .map(|s| s.gds(&sram_work_dir).display().to_string()),
    );
    let merge_str = merge_gds.join(" ");

    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            set_db timing_enable_simultaneous_setup_hold_mode true
            write_db {module}_FINAL -def -verilog
            set_db write_stream_virtual_connection false
            connect_global_net VDD -type net -net_base_name VPWR
            connect_global_net VDD -type net -net_base_name VPB
            connect_global_net VDD -type net -net_base_name vdd
            connect_global_net VSS -type net -net_base_name VGND
            connect_global_net VSS -type net -net_base_name VNB
            connect_global_net VSS -type net -net_base_name vss
            write_netlist {par_rundir}/{module}.lvs.v -top_module_first -top_module {module} -exclude_leaf_cells -phys -flat -exclude_insts_of_cells {{}}
            write_netlist {par_rundir}/{module}.sim.v -top_module_first -top_module {module} -exclude_leaf_cells -exclude_insts_of_cells {{}}
            write_stream -mode ALL -format stream -map_file /scratch/cs199-cbc/rivet/pdks/sky130/src/sky130_lefpin.map -uniquify_cell_names -merge {{ {merge_str} }} {par_rundir}/{module}.gds
            write_sdf -max_view {setup}.setup_view -min_view {hold}.hold_view -typical_view {typical}.extra_view {par_rundir}/{module}.par.sdf
            set_db extract_rc_coupled true
            extract_rc
            write_parasitics -spef_file {par_rundir}/{module}.{setup}.par.spef -rc_corner {setup}.setup_rc
            write_parasitics -spef_file {par_rundir}/{module}.{hold}.par.spef -rc_corner {hold}.hold_rc
            write_parasitics -spef_file {par_rundir}/{module}.{typical}.par.spef -rc_corner {typical}.extra_rc
            write_db post_write_design
            ln -sfn post_write_design latest
            "#
        ),
        name: "write_design".into(),
    }
}

pub enum FlatPinInfo {
    None,
    PinSyn(PathBuf),
    PinPar(PathBuf),
}

pub struct Sky130FlatFlow {
    pub module: String,
    pub syn: StepRef<GenusStep>,
    pub par: StepRef<InnovusStep>,
    pub submodules: Vec<SubmoduleInfo>,
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
    verilog_paths: &[PathBuf],
    srams: &[Sram22],
    sram_work_dir: &Path,
    dep_info: &[(&ModuleInfo, &Sky130FlatFlow)],
    submodules: Vec<SubmoduleInfo>,
    pin_info: &FlatPinInfo,
) -> GenusStep {
    let ss_100c_1v60 = MmmcCorner {
        name: "ss_100c_1v60".to_string(),
        corner_type: "setup".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib"),
        ],
        temperature: dec!(100.0),
    };
    let ff_n40c_1v95 = MmmcCorner {
        name: "ff_n40c_1v95".to_string(),
        corner_type: "hold".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib"),
        ],
        temperature: dec!(-40.0),
    };

    let tt_025c_1v80 = MmmcCorner {
        name: "tt_025c_1v80".to_string(),
        corner_type: "extra".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib"),
        ],
        temperature: dec!(25.0),
    };

    let syn_con = MmmcConfig {
        sdc_files: vec![work_dir.join("clock_pin_constraints.sdc")],

        corners: vec![
            ss_100c_1v60.clone(),
            ff_n40c_1v95.clone(),
            tt_025c_1v80.clone(),
        ],

        setup: vec![ss_100c_1v60.clone()],

        hold: vec![ff_n40c_1v95.clone(), tt_025c_1v80.clone()],

        dynamic: tt_025c_1v80.clone(),

        leakage: tt_025c_1v80.clone(),
    };
    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );
    let dir_submodules: Vec<SubmoduleInfo> = dep_info
        .iter()
        .map(|(module, _)| {
            submodules
                .iter()
                .find(|s| s.name == module.module_name)
                .cloned()
                .expect("Submodule info should already be present in submodules list")
        })
        .collect();

    let mut deps: Vec<Arc<dyn Step>> = dep_info
        .iter()
        .map(|(_module, flow)| Arc::new(flow.par.clone()) as Arc<dyn Step>)
        .collect();

    let is_hierarchical = !submodules.is_empty();

    // Check the rivet build directory first; only generate missing SRAMs.
    let missing_srams: Vec<Sram22> = srams
        .iter()
        .filter(|s| !s.ensure_generated(sram_work_dir))
        .cloned()
        .collect();

    if !missing_srams.is_empty() {
        fs::create_dir_all(sram_work_dir).expect("Failed to create sram directory");
        generate_compiler_script(&missing_srams, sram_work_dir)
            .expect("Failed to generate SRAM compiler script");
        let sram_compiler = Arc::new(BashStep::new(
            sram_work_dir.to_path_buf(),
            "generate_sram",
            module.as_str(),
            vec![],
        ));
        deps.push(sram_compiler);
    }

    GenusStep::new(
        work_dir,
        module,
        vec![
            set_default_options(),
            dont_avoid_lib_cells("ICGX1"),
            sky130_syn_read_design_files(
                work_dir,
                verilog_paths,
                syn_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
                Some(submodules.clone()),
                is_hierarchical,
                srams,
            ),
            elaborate(module),
            syn_init_design(module, Some(dir_submodules.clone())),
            power_intent(work_dir, &sky130_cadence_power_spec(module, dec!(1.8))),
            syn_generic(),
            syn_map(),
            add_tieoffs(),
            syn_write_design(module, ss_100c_1v60.clone(), is_hierarchical),
        ],
        matches!(pin_info, FlatPinInfo::PinSyn(_)),
        deps,
    )
}

pub fn sky130_par(
    pdk_root: &Path,
    work_dir: &PathBuf,
    module: &String,
    constraints: &Floorplan,
    netlist: &Path,
    srams: &[Sram22],
    sram_work_dir: &Path,
    submodules: Vec<SubmoduleInfo>,
    pin_info: &FlatPinInfo,
    syn_step: StepRef<GenusStep>,
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

    let ss_100c_1v60 = MmmcCorner {
        name: "ss_100c_1v60".to_string(),
        corner_type: "setup".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib"),
        ],
        temperature: dec!(100.0),
    };
    let ff_n40c_1v95 = MmmcCorner {
        name: "ff_n40c_1v95".to_string(),
        corner_type: "hold".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib"),
        ],
        temperature: dec!(-40.0),
    };

    let tt_025c_1v80 = MmmcCorner {
        name: "tt_025c_1v80".to_string(),
        corner_type: "extra".to_string(),
        libs: vec![
            pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib"),
        ],
        temperature: dec!(25.0),
    };

    let par_con = MmmcConfig {
        sdc_files: vec![work_dir.join("clock_pin_constraints.sdc")],

        corners: vec![
            ss_100c_1v60.clone(),
            ff_n40c_1v95.clone(),
            tt_025c_1v80.clone(),
        ],

        setup: vec![ss_100c_1v60.clone()],

        hold: vec![ff_n40c_1v95.clone(), tt_025c_1v80.clone()],

        dynamic: tt_025c_1v80.clone(),

        leakage: tt_025c_1v80.clone(),
    };

    fs::create_dir_all(work_dir.join("checkpoints/")).expect("Failed to create directory");

    let tlef = setup_techlef(
        work_dir,
        &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.tlef"),
    );

    let par_constraints = constraints.clone();

    InnovusStep::new(
        work_dir,
        module,
        vec![
            set_default_process(130),
            sky130_par_read_design_files(
                work_dir,
                module,
                netlist,
                par_con.clone(),
                &tlef,
                &pdk_root.join("sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef"),
                Some(submodules),
                srams,
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
            sky130_par_write_design(
                pdk_root,
                work_dir,
                module,
                srams,
                vec![
                    ss_100c_1v60.clone(),
                    ff_n40c_1v95.clone(),
                    tt_025c_1v80.clone(),
                ],
            ),
            write_ilm(
                work_dir,
                module,
                &layers[0],
                vec![
                    ss_100c_1v60.clone(),
                    ff_n40c_1v95.clone(),
                    tt_025c_1v80.clone(),
                ],
            ),
        ],
        matches!(pin_info, FlatPinInfo::PinPar(_)),
        vec![Arc::new(syn_step) as Arc<dyn Step>],
        false,
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
    // Shared SRAM cache dir for both syn and par under this module's build dir.
    let sram_work_dir = work_dir.join("sram");

    let mut all_submodules: Vec<SubmoduleInfo> = Vec::new();
    for (child_module, child_flow) in dep_info {
        let ilm = child_flow.par.get().ilm_path().to_path_buf();
        let lef = child_flow.par.get().lef_path().to_path_buf();
        let gds = child_flow.par.get().gds_path().to_path_buf();

        all_submodules.push(SubmoduleInfo {
            name: child_module.module_name.clone(),
            verilog_paths: child_module.verilog.clone(),
            gds,
            ilm,
            lef,
        });
        all_submodules.extend(child_flow.submodules.clone());
    }

    let syn_work_dir = work_dir.join("syn-rundir");
    let syn = sky130_syn(
        pdk_root,
        &syn_work_dir,
        &module.module_name,
        &module.verilog,
        &module.srams,
        &sram_work_dir,
        dep_info,
        all_submodules.clone(),
        &module.pin_info,
    );
    let syn_pointer = StepRef::new(syn);
    let par_work_dir = work_dir.join("par-rundir");
    let output_netlist_path = if !dep_info.is_empty() {
        syn_work_dir.join(format!("{}_noilm.mapped.v", module.module_name))
    } else {
        syn_work_dir.join(format!("{}.mapped.v", module.module_name))
    };

    let final_constraints = module.placement_constraints.clone();
    let par = sky130_par(
        pdk_root,
        &par_work_dir,
        &module.module_name,
        &final_constraints,
        &output_netlist_path,
        &module.srams,
        &sram_work_dir,
        all_submodules.clone(),
        &module.pin_info,
        syn_pointer.clone(),
    );
    let par_pointer = StepRef::new(par);
    Sky130FlatFlow {
        module: module.module_name.to_string(),
        syn: syn_pointer,
        par: par_pointer,
        submodules: all_submodules.clone(),
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
