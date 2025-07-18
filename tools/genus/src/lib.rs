use crate::fs::File;
use rivet::cadence::*;
use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

#[derive(Debug)]
pub struct Genus {
    pub work_dir: PathBuf,
}

impl Genus {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        Genus { work_dir: dir }
    }
    //concatenate steps to a tcl file, syn.tcl file, genus.tcl

    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Step>) -> io::Result<()> {
        let file_path = path.join("syn.tcl");
        let mut tcl_file = File::create(&file_path).expect("failed to create syn.tcl file");

        writeln!(tcl_file, "puts \"{}\"", "set_db hdl_error_on_blackbox true")?;
        writeln!(tcl_file, "set_db hdl_error_on_blackbox true")?;
        writeln!(tcl_file, "puts \"{}\"", "set_db max_cpus_per_server 12")?;
        writeln!(tcl_file, "set_db max_cpus_per_server 12")?;
        writeln!(tcl_file, "puts \"{}\"", "set_multi_cpu_usage -local_cpu 12")?;
        writeln!(tcl_file, "set_multi_cpu_usage -local_cpu 12")?;
        writeln!(
            tcl_file,
            "puts \"{}\"",
            "set_db super_thread_debug_jobs true"
        )?;
        writeln!(tcl_file, "set_db super_thread_debug_jobs true")?;
        writeln!(
            tcl_file,
            "puts \"{}\"",
            "set_db super_thread_debug_directory super_thread_debug"
        )?;
        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        for step in steps.into_iter() {
            writeln!(tcl_file, "puts\"{}\"", step.command.to_string())?;
            writeln!(tcl_file, "{}", step.command)?;
            if (step.checkpoint != None) {
                //generate tcl for checkpointing
                let mut checkpoint_command = String::new();

                writeln!(checkpoint_command, "write_db -to_file pre_{}", step.name);
                writeln!(tcl_file, "puts\"{}\"", checkpoint_command)?;
                writeln!(tcl_file, "{}", checkpoint_command)?;
            }
        }
        writeln!(tcl_file, "puts \"{}\"", "quit")?;
        writeln!(tcl_file, "quit")?;

        Ok(())
    }

    fn init_environment(
        &self,
        rtl: &PathBuf,
        top_module: &String,
        lef: &PathBuf,
        qrc: &PathBuf,
    ) -> Step {
        let mut command = String::new();

        //writeln!(&mut command, "hi {}", 1)?; this is example of what to do instead of push_str
        // --- Clock Gating Setup ---
        // Corresponds to the "synthesis.clock_gating_mode" == "auto" check

        writeln!(&mut command, "set_db lp_clock_gating_infer_enable  true");
        writeln!(
            &mut command,
            "set_db lp_clock_gating_prefix {}",
            "CLKGATE".to_string()
        );
        writeln!(&mut command, "set_db lp_insert_clock_gating  true");
        writeln!(&mut command, "set_db lp_clock_gating_register_aware true");

        // --- MMMC and Library Setup ---
        // This path is hardcoded for now, but you would generate and write this file at runtime

        //make a mmmc.tcl file in the work directory ie workdir/mmmc.tcl
        let mmmc_path = self.work_dir.join("mmmc.tcl");

        //then we call the mmmc script that writes tcl to the file in the provided file path
        generate_mmmc_script(&mmmc_path);

        writeln!(&mut command, "read_mmmc {}", mmmc_path.display());

        //need to hardcode the lef file path
        //lef_files = self.technology.read_libs([
        //hammer_tech.filters.lef_filter
        //], hammer_tech.HammerTechnologyUtils.to_plain_item)
        // In a real implementation, you would need to get the LEF files from your technology configuration
        writeln!(&mut command, "read_physical -lef {}", lef.display());

        //this command is ignored for our decoder
        // In a real implementation, you would need to get the QRC tech files from your technology configuration
        //writeln!("set_db qrc_tech_file {qrc_tech_files}");

        // --- HDL Input and Elaboration ---
        // In a real implementation, you would get the list of RTL files from your configuration
        // need to accept a parameter of the file path of our verilog
        // The rtl file path will be a parameter so we need to add a pathbuff
        writeln!(&mut command, "read_hdl -sv {}", rtl.display());

        //top module needs to be assigned to the name of our trl file so it is supposed to be
        //"decoder"
        writeln!(&mut command, "elaborate {}", top_module);
        writeln!(&mut command, "init_design -top {}", top_module);

        // --- Constraints and Design Settings ---
        writeln!(&mut command, "report_timing -lint -verbose");
        // In a real implementation, you would read a UPF or CPF file!()
        // find what are the power_commands in hammer
        //writeln!("read_upf {power_intent_file}");
        //writeln!("apply_power_intent -summary");
        //
        //
        // Need to create a function that makes the powerspec
        writeln!(&mut command, "read_power_intent -cpf /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/power_spec.cpf");

        writeln!(&mut command, "apply_power_intent -summary");
        writeln!(&mut command, "commit_power_intent");

        writeln!(&mut command, "set_db root: .auto_ungroup none");
        // This setting would come from your configuration and are not necessary for the decoder
        //
        //writeln!("set_db phys_flow_effort high");
        //writeln!("set_db opt_spatial_effort extreme");

        // --- "Don't Use" Cells ---
        // In a real implementation, you would generate a list of "don't use" cells
        //writeln!("set_dont_use {dont_use_cells}");

        Step {
            name: "init_environment".to_string(),
            command: command,
            checkpoint: true,
        }
    }

    fn predict_floorplan(innovus_path: &PathBuf) -> Step {
        let mut command = String::new();
        // In a real implementation, this would be based on a setting like
        // `synthesis.genus.phys_flow_effort`. This example assumes "high" effort.

        writeln!(&mut command, "set_db invs_temp_dir temp_invs");
        // The innovus binary path would be a configurable parameter.
        writeln!(
            &mut command,
            "set_db innovus_executable {}",
            innovus_path.display()
        );
        writeln!(
            &mut command,
            "set_db predict_floorplan_enable_during_generic true"
        );
        writeln!(&mut command, "set_db physical_force_predict_floorplan true");
        writeln!(&mut command, "set_db predict_floorplan_use_innovus true");

        writeln!(&mut command, "predict_floorplan");

        Step {
            name: "predict_floorplan".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn syn_generic() -> Step {
        let mut command = String::new();

        // Based on `synthesis.genus.phys_flow_effort`.
        // if synthesis.genus.phys_flow_effort.lower() == "none"
        writeln!(&mut command, "syn_generic");

        //else append "syn_generic -physical"

        Step {
            name: "syn_generic".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn syn_map() -> Step {
        let mut command = String::new();
        writeln!(&mut command, "syn_map");

        // This corresponds to `synthesis.genus.phys_flow_effort` = "high"
        //writeln!("syn_opt -spatial");

        Step {
            name: "syn_map".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn add_tieoffs() -> Step {
        let mut command = String::new();

        writeln!(&mut command, "set_db message:WSDF-201 .max_print 20");
        writeln!(&mut command, "set_db use_tiehilo_for_const duplicate");

        // The cell names {TIE_HI_CELL} and {TIE_LO_CELL} would be dynamically
        // retrieved from the technology configuration.
        //# If MMMC corners specified, get the single lib cell for the active analysis view
        //Else, Genus will complain that multiple objects match for the cell name
        //corners = self.get_mmmc_corners()
        //if corners:
        //    self.verbose_append("set ACTIVE_SET [string map { .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set } [get_db [get_analysis_views] .name]]")
        //    self.verbose_append("set HI_TIEOFF [get_db base_cell:{TIE_HI_CELL} .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]".format(TIE_HI_CELL=tie_hi_cell))
        //    self.verbose_append("set LO_TIEOFF [get_db base_cell:{TIE_LO_CELL} .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]".format(TIE_LO_CELL=tie_lo_cell))
        //    self.verbose_append("add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose")
        //else:
        //    self.verbose_append("add_tieoffs -high {HI_TIEOFF} -low {LO_TIEOFF} -max_fanout 1 -verbose".format(HI_TIEOFF=tie_hi_cell, LO_TIEOFF=tie_lo_cell))

        //right now this is hardcoded since we need some parameters from the mmmc corners and teh
        //mmmc libraries
        //
        //
        let tie_hi_cell = "TIEHI";
        let tie_lo_cell = "TIELO";
        if true {
            writeln!(&mut command, "set ACTIVE_SET [string map {{ .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set }} [get_db [get_analysis_views] .name]]");
            writeln!(&mut command, "set HI_TIEOFF [get_db base_cell:{} .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]", tie_hi_cell);
            writeln!(&mut command, "set LO_TIEOFF [get_db base_cell:{} .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]", tie_lo_cell);
            writeln!(
                &mut command,
                "add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose"
            );
        } else {
            writeln!(
                &mut command,
                "add_tieoffs -high {{TIE_HI_CELL}} -low {{LO_LO_CELL}} -max_fanout 1 -verbose",
            );
        }

        Step {
            name: "add_tieoffs".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn write_regs() -> Step {
        let mut command = String::new();
        // This part of the command would be dynamically generated by helper
        // functions like `child_modules_tcl()` and `write_regs_tcl()` in the
        // original Python code to find and format register information.
        writeln!(&mut command, "set regs [find / -seq_cells]");
        writeln!(&mut command, "set reg_paths [get_db $regs .name]");
        writeln!(&mut command, "set fp [open \"find_regs_paths.json\" \"w\"]");
        writeln!(&mut command, "puts $fp $reg_paths");
        writeln!(&mut command, "close $fp");

        Step {
            name: "write_regs".to_string(),
            command,
            checkpoint: false, // This step doesn't modify the design itself
        }
    }
    fn generate_reports() -> Step {
        let mut command = String::new();
        writeln!(&mut command, "write_reports -directory reports -tag final");
        //writeln!("report_ple > reports/final_ple.rpt");
        writeln!(
            &mut command,
            "report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt",
        );

        Step {
            name: "generate_reports".to_string(),
            command,
            checkpoint: false,
        }
    }

    fn write_outputs(&self, top_module: &String, corners: Corner) -> Step {
        let mut command = String::new();

        // The filenames would use a variable for the top module name.
        //writeln!("write_hdl > {top_module}.mapped.v");
        //
        let mapped_v_path = self.work_dir.join("{top_module}.mapped.v");
        writeln!(&mut command, "write_hdl > {}", mapped_v_path.display());

        //writeln!("write_hdl -exclude_ilm > {top_module}_noilm.mapped.v");
        //writeln!("write_sdc -view {setup_view_name} > {top_module}.mapped.sdc");
        //writeln!("write_sdf > {top_module}.mapped.sdf");
        //
        //// Corresponds to `phys_flow_effort` != "none"
        //writeln!("write_db -common");
        //
        // change this tcl from hardcoded

        // verbose_append("write_template -full -outfile {}.mapped.scr".format(top))
        writeln!(
            &mut command,
            "write_template -full -outfile {}.mapped.scr",
            top_module
        );

        //view_name="{cname}.setup_view".format(cname=next(filter(lambda c: c.type is MMMCCornerType.Setup, corners)).name)
        let view_name = "{corners[0].name}.setup_view";
        let mapped_sdc_path = self.work_dir.join("{top_module}.mapped.sdc");
        //verbose_append("write_sdc -view {view} > {file}".format(view=view_name, file=self.mapped_sdc_path))
        writeln!(
            &mut command,
            "write_sdc -view {} > {}",
            view_name,
            mapped_sdc_path.display()
        );

        //verbose_append("write_sdf > {run_dir}/{top}.mapped.sdf".format(run_dir=self.run_dir, top=top))
        writeln!(
            &mut command,
            "write_sdf > {}/{}.mapped.sdf",
            self.work_dir.display(),
            top_module
        );
        //verbose_append("write_design -gzip_files {top}".format(top=top))
        writeln!(&mut command, "write_design -gzip_files {}", top_module);

        Step {
            name: "write_outputs".to_string(),
            command,
            checkpoint: false,
        }
    }

    fn run_genus() -> bool {
        true
    }
}

impl Tool for Genus {
    //fn work_dir(&self) -> PathBuf {
    //    self.work_dir.clone()
    //}
    // genus -files syn.tcl -no_gui
    fn invoke(&self, steps: Vec<Step>) {
        let mut tcl_path = PathBuf::new();
        tcl_path.push(self.work_dir);
        tcl_path.push("syn.tcl");

        self.make_tcl_file(&tcl_path, steps);

        let status = Command::new("genus")
            .args(["-files", tcl_path.into_os_string().into_string(), "-no_gui"])
            .current_dir(&self.work_dir)
            .status()
            .expect("Failed to execute syn.tcl");

        if !status.success() {
            eprintln!("Failed to execute syn.tcl");
            panic!("Stopped flow");
        }
    }

    //fn checkpoint(&self, step: Step) -> PathBuf {
    //    let mut ret = PathBuf::new();
    //    ret.push(self.work_dir);
    //    ret.push(format!("{}.checkpoint", step.name));
    //
    //    return ret;
    //}

    // fn write_checkpoint(&self, path: &PathBuf) -> Step {
    //     let checkpoint_command = format!("write_db -to_file {}", path.to_str().unwrap());
    //     println!("  - Writing checkpoint w command: {}", checkpoint_command);

    //     Step {
    //         name: format!("write_checkpoint_to_{}", path.to_str().unwrap()).to_string(),
    //         command: checkpoint_command,
    //         checkpoint: true,
    //     }
    // }

    // fn read_checkpoint(&self, path: &PathBuf) -> Step {
    //     let command = format!("read_db", path.to_str().unwrap());
    //     println!("  - Reading checkpoint with command: {}", command);
    //     Step {
    //         name: "read_checkpoint".to_string(),
    //         command,
    //         checkpoint: false,
    //     }
    // }
}

#[cfg(test)]

mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rivet::flow::{FlowNode, Step, Tool};

    use crate::Genus;

    #[test]
    fn test_basic_flow() {
        let genus: Arc<dyn Tool> = Arc::new(Genus::new(PathBuf::from(".")));
        let genus_steps = vec![
            Genus::init_environment(),
            Genus::syn_generic(),
            Genus::syn_map(),
            Genus::add_tieoffs(),
            Genus::generate_reports(),
            Genus::write_outputs(),
        ];

        let basic = FlowNode {
            name: "Genus".to_string(),
            tool: Arc::clone(&genus),
            steps: genus_steps,
            deps: vec![],
        };

        genus.invoke(basic.steps);
    }
}
