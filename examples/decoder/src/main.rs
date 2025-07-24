use clap::Parser;
use rivet::flow::{Config, ToolConfig};
use sky130::reference_flow;
use std::path::PathBuf;
use std::fs;
use toml;
use std::collections::HashMap;

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
struct CliArgs {
    /// The name of the final flow node to execute (e.g., 'par').
    node: String,
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// Path to the Rivet TOML configuration file.
    #[arg(long)]
    config: PathBuf,
}

fn main() {
    // let args = CliArgs::parse();
    // let config_str = fs::read_to_string(&args.config).expect("Failed to read config file");
    // let config: Config = toml::from_str(&config_str).expect("Failed to parse config file");
    // let work_dir = args.work_dir.unwrap_or("build".into());
    let mut tools = HashMap::new();
    let conf =  ToolConfig {start: None, stop: Some("write_design".to_string()), pin: None};
    tools.insert("Genus".to_string(), conf.clone());
    let config = Config {tools : tools.clone() };
    let flow = reference_flow(PathBuf::from("scratch/cs199-cbc/rivet/examples/decoder/src"));
    flow.execute("syn", &config);
}
