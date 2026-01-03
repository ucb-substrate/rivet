use regex::Regex;
use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::{Checkpoint, MmmcConfig, MmmcCorner, SubmoduleInfo, Substep, mmmc, sdc};
use fs::File;
use indoc::formatdoc;
use rivet::Step;
use std::sync::Arc;

/// Defines the Genus synthesis step subflow
#[derive(Debug, Clone)]
pub struct GenusStep {
    pub work_dir: PathBuf,
    pub module: String,
    pub substeps: Vec<Substep>,
    pub pinned: bool,
    pub start_checkpoint: Option<Checkpoint>,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl GenusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        module: impl Into<String>,
        steps: Vec<Substep>,
        pinned: bool,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let modul = module.into();
        GenusStep {
            work_dir: dir,
            module: modul,
            substeps: steps,
            pinned,
            start_checkpoint: None,
            dependencies: deps,
        }
    }

    /// Generates the tcl file for synthesis
    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Substep>) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create syn.tcl parent directory");
        }

        let mut tcl_file = File::create(path).expect("failed to create syn.tcl file");

        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        if let Some(checkpoint) = &self.start_checkpoint {
            writeln!(tcl_file, "read_db {}", checkpoint.path.display()).expect("Failed to write");
        }

        for step in steps.into_iter() {
            println!("\n--> Parsing step: {}\n", step.name);

            writeln!(tcl_file, "{}", step.command)?;
            if step.checkpoint {
                let checkpoint_file = self.work_dir.join(format!("post_{}", step.name.clone()));

                writeln!(tcl_file, "write_db -to_file {}", checkpoint_file.display())?;
            }
        }
        writeln!(tcl_file, "quit")?;

        println!("\nFinished creating tcl file\n");
        Ok(())
    }

    /// Inserts a custom command as a substep in the synthesis flow
    pub fn add_hook(&mut self, name: &str, tcl: &str, index: usize, checkpointed: bool) {
        self.substeps.insert(
            index,
            Substep {
                name: name.to_string(),
                command: tcl.to_string(),
                checkpoint: checkpointed,
            },
        );
    }

    /// Replaces a specfic substep in the synthesis flow with a new command
    pub fn replace_hook(
        &mut self,
        new_substep_name: &str,
        tcl: &str,
        replaced_substep_name: &str,
        checkpointed: bool,
    ) {
        if let Some(index) = self
            .substeps
            .iter()
            .position(|s| s.name == replaced_substep_name)
        {
            self.substeps[index] = Substep {
                name: new_substep_name.to_string(),
                command: tcl.to_string(),
                checkpoint: checkpointed,
            };
        }
    }

    pub fn netlist(&self) -> PathBuf {
        self.work_dir.join(format!("{}.mapped.v", self.module))
    }

    /// Assigns the starting checkpoint of the synthesis flow
    pub fn add_checkpoint(mut self, name: String, checkpoint_path: PathBuf) {
        self.start_checkpoint = Some(Checkpoint {
            name,
            path: checkpoint_path,
        });
    }
}

impl Step for GenusStep {
    fn execute(&self) {
        let tcl_path = self.work_dir.clone().join("syn.tcl");
        let mut substeps = self.substeps.clone();
        if let Some(checkpoint) = &self.start_checkpoint {
            let slice_index = self
                .substeps
                .iter()
                .position(|s| s.name == checkpoint.name)
                .expect("Failed to find checkpoint name");
            substeps = self.substeps[slice_index..].to_vec();
        }

        self.make_tcl_file(&tcl_path, substeps)
            .expect("Failed to create syn.tcl");

        let status = Command::new("genus")
            .args(["-f", tcl_path.to_str().unwrap(), "-no_gui", "-batch"])
            .current_dir(self.work_dir.clone())
            .status()
            .expect("Failed to execute syn.tcl");

        if !status.success() {
            eprintln!("Failed to execute syn.tcl");
            panic!("Stopped flow");
        }
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }
}

pub fn set_default_options() -> Substep {
    Substep {
        checkpoint: false,
        name: "set_default_options".into(),
        command: r#"
            set_db hdl_error_on_blackbox true
            set_db max_cpus_per_server 12
            set_multi_cpu_usage -local_cpu 12
            set_db super_thread_debug_jobs true
            set_db super_thread_debug_directory super_thread_debug
            set_db lp_clock_gating_infer_enable  true
            set_db lp_clock_gating_prefix  {CLKGATE}
            set_db lp_insert_clock_gating  true
            set_db lp_clock_gating_register_aware true
            set_db root: .auto_ungroup none
        "#
        .into(),
    }
}

pub fn dont_avoid_lib_cells(base_name: &str) -> Substep {
    Substep {
        checkpoint: false,
        name: format!("dont_avoid_lib_cells_{base_name}"),
        command: formatdoc!(
            r#"set_db [get_db lib_cells -if {{.base_name == {base_name}}}] .avoid false"#
        ),
    }
}

/// Reads the module verilog, mmmc.tcl, pdk lefs, ilms paths, and sdc constraints
pub fn syn_read_design_files(
    work_dir: &Path,
    verilog_paths: &[PathBuf],
    mmmc_conf: MmmcConfig,
    tlef: &Path,
    pdk_lef: &Path,
    submodules: Option<Vec<SubmoduleInfo>>,
    is_hierarchical: bool,
) -> Substep {
    let mut sdc_file =
        File::create(work_dir.join("clock_pin_constraints.sdc")).expect("failed to create file");
    writeln!(sdc_file, "{}", sdc()).expect("Failed to write");
    let mmmc_tcl = mmmc(mmmc_conf);
    let mmmc_tcl_path = work_dir.to_path_buf().join("mmmc.tcl");
    let _ = fs::write(&mmmc_tcl_path, mmmc_tcl);

    let mut lefs_vec = vec![tlef.display().to_string(), pdk_lef.display().to_string()];

    if let Some(submodule_lefs) = &submodules {
        lefs_vec.extend(
            submodule_lefs
                .iter()
                .map(|p| p.lef.to_string_lossy().to_string()),
        );
    }

    let lefs: String = lefs_vec.join(" ");

    let mut command = formatdoc!(
        r#"
            read_mmmc {}
            "#,
        mmmc_tcl_path.display(),
    );

    let verilog_files: Vec<String> = verilog_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    if let Some(submodule_vec) = &submodules {
        for submodule in submodule_vec {
            writeln!(
                command,
                "read_ilm -basename {}/mmmc/ilm_data/{}/{}_postRoute -module_name {}",
                submodule.ilm.display(),
                submodule.name,
                submodule.name,
                submodule.name,
            )
            .unwrap();
        }
    }
    let submodule_names: Vec<String> = submodules
        .as_ref()
        .map(|v| v.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();

    let mut final_verilog_files = Vec::new();

    if is_hierarchical {
        for verilog in &verilog_files {
            let new_path =
                remove_hierarchical_submodules(Path::new(verilog), work_dir, &submodule_names)
                    .expect("Failed to remove hierarchical submodules");
            final_verilog_files.push(new_path.to_string_lossy().to_string());
        }
    } else {
        final_verilog_files = verilog_files;
    }

    let all_verilog = final_verilog_files.join(" ");

    writeln!(
        command,
        r#"
        read_physical -lef {{ {} }}
        read_hdl -sv {{ {} }}
        "#,
        lefs, all_verilog,
    )
    .unwrap();

    Substep {
        checkpoint: false,
        command,
        name: "read_design_files".into(),
    }
}

pub fn elaborate(module: &String) -> Substep {
    Substep {
        checkpoint: false,
        command: format!("elaborate {}", module),
        name: "elaborate".to_string(),
    }
}

pub fn syn_init_design(module: &String, submodules: Option<Vec<SubmoduleInfo>>) -> Substep {
    let mut command = String::new();
    if let Some(submodule_ilms) = &submodules {
        for ilm in submodule_ilms {
            writeln!(
                command,
                "set_db module:{}/{} .preserve true",
                module, ilm.name
            )
            .unwrap();
        }
    }
    writeln!(command, "init_design -top {}", module).unwrap();
    Substep {
        checkpoint: false,
        command,
        name: "init_design".to_string(),
    }
}

/// Write power_spec.cpf and run power_intent TCL commands.
pub fn power_intent(work_dir: &Path, power_spec: &String) -> Substep {
    let power_spec_file_path = work_dir.join("power_spec.cpf");

    let mut power_spec_file = File::create(&power_spec_file_path).expect("failed to create file");
    writeln!(power_spec_file, "{}", power_spec).expect("Failed to write");
    let power_spec_file_string = power_spec_file_path.display();
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
        read_power_intent -cpf {power_spec_file_string}
        apply_power_intent -summary
        commit_power_intent
        "#
        ),
        name: "power_intent".into(),
    }
}

pub fn syn_generic() -> Substep {
    Substep {
        checkpoint: true,
        command: "syn_generic".to_string(),
        name: "syn_generic".to_string(),
    }
}

pub fn syn_map() -> Substep {
    Substep {
        checkpoint: true,
        command: "syn_map".to_string(),
        name: "syn_map".to_string(),
    }
}

pub fn add_tieoffs() -> Substep {
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"set_db message:WSDF-201 .max_print 20
        set_db use_tiehilo_for_const duplicate
        set ACTIVE_SET [string map {{ .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set }} [get_db [get_analysis_views] .name]]
        set HI_TIEOFF [get_db base_cell:TIEHI .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]
        set LO_TIEOFF [get_db base_cell:TIELO .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]
        add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose
        "#
        ),
        name: "add_tieoffs".into(),
    }
}

pub fn syn_write_design(module: &str, sdc_corner: MmmcCorner, is_hierarchical: bool) -> Substep {
    let module = module.to_owned();
    let corner = sdc_corner.name.clone();
    let corner_type = sdc_corner.corner_type.clone();

    let write_hdl = if is_hierarchical {
        format!("write_hdl -exclude_ilm > {module}_noilm.mapped.v")
    } else {
        format!("write_hdl > {module}.mapped.v")
    };

    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            set write_cells_ir "./find_regs_cells.json"
            set write_cells_ir [open $write_cells_ir "w"]
            puts $write_cells_ir "\["

            set refs [get_db [get_db lib_cells -if .is_sequential==true] .base_name]

            set len [llength $refs]

            for {{set i 0}} {{$i < [llength $refs]}} {{incr i}} {{
                if {{$i == $len - 1}} {{
                    puts $write_cells_ir "    \"[lindex $refs $i]\""
                }} else {{
                    puts $write_cells_ir "    \"[lindex $refs $i]\","
                }}
            }}

            puts $write_cells_ir "\]"
            close $write_cells_ir
            set write_regs_ir "./find_regs_paths.json"
            set write_regs_ir [open $write_regs_ir "w"]
            puts $write_regs_ir "\["

            set regs [get_db [get_db [all_registers -edge_triggered -output_pins] -if .direction==out] .name]

            set len [llength $regs]

            for {{set i 0}} {{$i < [llength $regs]}} {{incr i}} {{
                #regsub -all {{/}} [lindex $regs $i] . myreg
                set myreg [lindex $regs $i]
                if {{$i == $len - 1}} {{
                    puts $write_regs_ir "    \"$myreg\""
                }} else {{
                    puts $write_regs_ir "    \"$myreg\","
                }}
            }}

            puts $write_regs_ir "\]"

            close $write_regs_ir
            write_reports -directory reports -tag final
            report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt

            {write_hdl}
            write_template -full -outfile {module}.mapped.scr
            write_sdc -view {corner}.{corner_type}_view > {module}.mapped.sdc
            write_sdf > {module}.mapped.sdf
            write_design -gzip_files {module}
        "#
        ),
        name: "write_design".into(),
    }
}

pub fn remove_hierarchical_submodules(
    source_path: &Path,
    work_dir: &Path,
    submodules: &[String],
) -> io::Result<PathBuf> {
    let content = fs::read_to_string(source_path)?;
    let mut new_content = content.clone();

    for submodule in submodules {
        let re_str = format!(
            r"(?s)\bmodule\s+{}\b.*?\bendmodule\b",
            regex::escape(submodule)
        );
        let re = Regex::new(&re_str).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        new_content = re.replace_all(&new_content, "").to_string();
    }

    let file_stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("design");
    let new_filename = format!("{}_no_submodules.v", file_stem);
    let new_path = work_dir.join(new_filename);

    fs::write(&new_path, new_content)?;

    Ok(new_path)
}
