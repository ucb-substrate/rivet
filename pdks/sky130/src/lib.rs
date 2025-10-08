use std::fmt::Write as FmtWrite;
use std::{
    collections::HashMap,
    fs,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use cadence::cadence::{mmmc, sdc, MmmcConfig, MmmcCorner, Substep};
use cadence::genus::{dont_avoid_lib_cells, set_default_options, GenusStep};
use cadence::innovus::{set_default_process, InnovusStep, Layer, PinAssignment};
use indoc::formatdoc;
use rivet::Step;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn sky130_connect_nets() -> Substep {
    Substep {
        command: formatdoc!(
            r#"
            connect_global_net VDD -type pg_pin -pin_base_name VPWR -all -auto_tie -netlist_override
            connect_global_net VDD -type net    -net_base_name VPWR -all -netlist_override
            connect_global_net VDD -type pg_pin -pin_base_name VPB -all -auto_tie -netlist_override
            connect_global_net VDD -type net    -net_base_name VPB -all -netlist_override
            connect_global_net VDD -type pg_pin -pin_base_name vdd -all -auto_tie -netlist_override
            connect_global_net VDD -type net    -net_base_name vdd -all -netlist_override
            connect_global_net VSS -type pg_pin -pin_base_name VGND -all -auto_tie -netlist_override
            connect_global_net VSS -type net    -net_base_name VGND -all -netlist_override
            connect_global_net VSS -type pg_pin -pin_base_name VNB -all -auto_tie -netlist_override
            connect_global_net VSS -type net    -net_base_name VNB -all -netlist_override
            connect_global_net VSS -type pg_pin -pin_base_name vss -all -auto_tie -netlist_override
            connect_global_net VSS -type net    -net_base_name vss -all -netlist_override
            "#
        ),
        name: "sky130_connect_nets".into(),
    }
}

pub fn setup_techlef(working_directory: &PathBuf, lef_file: &PathBuf) -> PathBuf {
    let cache_dir = working_directory.join("tech-sky130-cache");
    fs::create_dir(&cache_dir).expect("failed to create directory");

    // Dynamically get the file stem from the input LEF file
    let file_stem = lef_file
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("failed to create file stem");

    let tlef_path = cache_dir.join(format!("{}.tlef", file_stem));

    // Set up buffered reader and writer for efficiency
    let reader = BufReader::new(File::open(lef_file).expect("failed to read file"));
    let mut techlef = BufWriter::new(File::create(&tlef_path).expect("failed to write to file"));

    let licon = r#"
LAYER licon
    TYPE CUT ;
END licon
"#;
    let nwell = r#"
LAYER nwell
    TYPE MASTERSLICE ;
END nwell
LAYER pwell
    TYPE MASTERSLICE ;
END pwell
LAYER li1
    TYPE MASTERSLICE ;
END li1
"#;

    // Iterate over each line, handling potential I/O errors
    for line_result in reader.lines() {
        let line = line_result.expect("failed to read line from file");
        writeln!(techlef, "{}", line).expect("failed to fetch line");

        // Check the content of the line to insert new blocks
        if line.trim() == "END pwell" {
            techlef
                .write_all(licon.as_bytes())
                .expect("failed to write");
        }
        if line.trim() == "END poly" {
            // Write each byte slice separately instead of trying to add them
            techlef
                .write_all(nwell.as_bytes())
                .expect("failed to write");
            techlef
                .write_all(licon.as_bytes())
                .expect("failed to write");
        }
    }
    tlef_path
}
