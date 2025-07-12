use crate::fs::File;
use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::io::prelude::*;
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
            //if step name is in a hashset of helper functions run the helper function instead

            writeln!(tcl_file, "puts\"{}\"", step.command.to_string())?;
            writeln!(tcl_file, "{}", step.command)?;
            if (step.checkpoint) {
                //generate tcl for checkpointing
                self.write_checkpoint(path);
            }
        }
        writeln!(tcl_file, "puts \"{}\"", "quit")?;
        writeln!(tcl_file, "quit")?;

        Ok(())
    }

    fn init_environment(rtl: &PathBuf, top_module: &String) -> Step {
        let mut command_init = String::new();

        //writeln!(&mut command_init, "hi {}", 1)?; this is example of what to do instead of push_str
        // --- Clock Gating Setup ---
        // Corresponds to the "synthesis.clock_gating_mode" == "auto" check
        command_init.push_str("set_db lp_clock_gating_infer_enable  true\n");
        command_init.push_str("set_db lp_clock_gating_prefix {CLKGATE}\n");
        command_init.push_str("set_db lp_insert_clock_gating  true\n");
        command_init.push_str("set_db lp_clock_gating_register_aware true\n");

        // --- MMMC and Library Setup ---
        // This path is hardcoded for now, but you would generate and write this file at runtime
        command_init.push_str(
            "read_mmmc /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/mmmc.tcl\n",
        );

        //need to hardcode the lef file path
        // In a real implementation, you would need to get the LEF files from your technology configuration
        command_init.push_str("read_physical -lef {lef_files}\n");

        //this command is ignored for our decoder
        // In a real implementation, you would need to get the QRC tech files from your technology configuration
        //command_init.push_str("set_db qrc_tech_file {qrc_tech_files}\n");

        // --- HDL Input and Elaboration ---
        // In a real implementation, you would get the list of RTL files from your configuration
        // need to accept a parameter of the file path of our verilog
        // The rtl file path will be a parameter so we need to add a pathbuff
        command_init.push_str("read_hdl -sv {rtl_files}\n");

        //top module needs to be assigned to the name of our trl file so it is supposed to be
        //"decoder"
        command_init.push_str("elaborate {top_module}\n");
        command_init.push_str("init_design -top {top_module}\n");

        // --- Constraints and Design Settings ---
        command_init.push_str("report_timing -lint -verbose\n");
        // In a real implementation, you would read a UPF or CPF file!()
        // find what are the power_commands in hammer
        //command_init.push_str("read_upf {power_intent_file}\n");
        //command_init.push_str("apply_power_intent -summary\n");
        //
        //
        // Need to create a function that makes the powerspec
        command_init.push_str("read_power_intent -cpf /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/power_spec.cpf\n");

        command_init.push_str("apply_power_intent -summary\n");
        command_init.push_str("commit_power_intent\n");

        command_init.push_str("set_db root: .auto_ungroup none\n");
        // This setting would come from your configuration and are not necessary for the decoder
        //
        //command_init.push_str("set_db phys_flow_effort high\n");
        //command_init.push_str("set_db opt_spatial_effort extreme\n");

        // --- "Don't Use" Cells ---
        // In a real implementation, you would generate a list of "don't use" cells
        //command_init.push_str("set_dont_use {dont_use_cells}\n");

        Step {
            name: "init_environment".to_string(),
            command: command_init,
            checkpoint: true,
        }
    }

    fn predict_floorplan() -> Step {
        let mut command = String::new();
        // In a real implementation, this would be based on a setting like
        // `synthesis.genus.phys_flow_effort`. This example assumes "high" effort.

        command.push_str("set_db invs_temp_dir temp_invs\n");
        // The innovus binary path would be a configurable parameter.
        command.push_str("set_db innovus_executable {innovus_bin_path}\n");
        command.push_str("set_db predict_floorplan_enable_during_generic true\n");
        command.push_str("set_db physical_force_predict_floorplan true\n");
        command.push_str("set_db predict_floorplan_use_innovus true\n");

        command.push_str("predict_floorplan\n");

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
        command.push_str("syn_generic\n");

        //else append "syn_generic -physical"

        Step {
            name: "syn_generic".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn syn_map() -> Step {
        let mut command = String::new();
        command.push_str("syn_map\n");

        // This corresponds to `synthesis.genus.phys_flow_effort` = "high"
        //command.push_str("syn_opt -spatial\n");

        Step {
            name: "syn_map".to_string(),
            command,
            checkpoint: true,
        }
    }

    fn add_tieoffs() -> Step {
        let mut command = String::new();

        command.push_str("set_db message:WSDF-201 .max_print 20\n");
        command.push_str("set_db use_tiehilo_for_const duplicate\n");

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
        if true {
            command.push_str("set ACTIVE_SET [string map { .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set } [get_db [get_analysis_views] .name]]");
            command.push_str("set HI_TIEOFF [get_db base_cell:TIEHI .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]");
            command.push_str("set LO_TIEOFF [get_db base_cell:TIELO .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]");
            command.push_str("add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose");
        } else {
            command.push_str(
                "add_tieoffs -high {TIE_HI_CELL} -low {LO_LO_CELL} -max_fanout 1 -verbose\n",
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
        command.push_str("set regs [find / -seq_cells]\n");
        command.push_str("set reg_paths [get_db $regs .name]\n");
        command.push_str("set fp [open \"find_regs_paths.json\" \"w\"]\n");
        command.push_str("puts $fp $reg_paths\n");
        command.push_str("close $fp\n");

        Step {
            name: "write_regs".to_string(),
            command,
            checkpoint: false, // This step doesn't modify the design itself
        }
    }
    fn generate_reports() -> Step {
        let mut command = String::new();
        command.push_str("write_reports -directory reports -tag final\n");
        //command.push_str("report_ple > reports/final_ple.rpt\n");
        command.push_str(
            "report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt\n",
        );

        Step {
            name: "generate_reports".to_string(),
            command,
            checkpoint: false,
        }
    }

    fn write_outputs(top_module: &String) -> Step {
        let mut command = String::new();

        // The filenames would use a variable for the top module name.
        //command.push_str("write_hdl > {top_module}.mapped.v\n");
        command.push_str("write_hdl > /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/decoder.mapped.v");

        //command.push_str("write_hdl -exclude_ilm > {top_module}_noilm.mapped.v\n");
        //command.push_str("write_sdc -view {setup_view_name} > {top_module}.mapped.sdc\n");
        //command.push_str("write_sdf > {top_module}.mapped.sdf\n");
        //
        //// Corresponds to `phys_flow_effort` != "none"
        //command.push_str("write_db -common\n");
        //
        // change this tcl from hardcoded

        // verbose_append("write_template -full -outfile {}.mapped.scr".format(top))
        command.push_str("write_template -full -outfile decoder.mapped.scr");

        //verbose_append("write_sdc -view {view} > {file}".format(view=view_name, file=self.mapped_sdc_path))
        command.push_str("write_sdc -view ss_100C_1v60.setup_view > /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/decoder.mapped.sdc");

        //verbose_append("write_sdf > {run_dir}/{top}.mapped.sdf".format(run_dir=self.run_dir, top=top))
        command.push_str("write_sdf > /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/syn-rundir/decoder.mapped.sdf");
        //verbose_append("write_design -gzip_files {top}".format(top=top))
        command.push_str("write_design -gzip_files decoder");

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
    fn work_dir(&self) -> PathBuf {
        self.work_dir.clone()
    }
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

    fn checkpoint(&self, step: Step) -> PathBuf {
        let mut ret = PathBuf::new();
        ret.push(self.work_dir);
        ret.push(format!("{}.checkpoint", step.name));

        return ret;
    }

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
