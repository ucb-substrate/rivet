use clap::Parser;
use dummy::dummytool::DummyTool;
use serde::Deserialize;
use shammer::{FlowManager, FlowNode, Step, Tool};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize, Debug)]
struct Config {
    #[serde(flatten)]
    tools: HashMap<String, ToolConfig>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
struct ToolConfig {
    start: Option<String>,
    stop: Option<String>,
    checkpoint: Option<PathBuf>,
    pin: Option<bool>,
    output_dir: Option<PathBuf>,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    flow: PathBuf,
    #[argo()]
    step: Option<String>,
}

fn get_steps_for_tool(tool_name: &str, all_steps: &[Step], config: &Config) -> Vec<Step> {
    let tool_config = match config.tools.get(tool_name) {
        Some(tc) => tc,
        None => return all_steps.to_vec(), // No config for this tool, run all steps
    };

    let start_index = tool_config
        .start
        .as_ref()
        .and_then(|start_name| all_steps.iter().position(|s| &s.name == start_name))
        .unwrap_or(0);

    let stop_index = tool_config
        .stop
        .as_ref()
        .and_then(|stop_name| all_steps.iter().position(|s| &s.name == stop_name))
        .unwrap_or(all_steps.len());

    all_steps[start_index..stop_index].to_vec()
}
// Import your flow modules here
mod example;
mod sky130;
// Add other flow modules as needed

/// Returns the Flow for a given flow file stem.
fn get_flow_by_stem(stem: &str) -> Flow {
    match stem {
        "sky130" => sky130::get_flow(),
        "example" => example::get_flow(),
        "custom" => custom::get_flow(),
        // Add other flows here
        _ => panic!("Unknown flow: {}", stem),
    }
}

fn main() {
    let cli = Cli::parse();

    let config_str = fs::read_to_string(&cli.config).expect("Failed to read config file");
    let config: Config = toml::from_str(&config_str).expect("Failed to parse config file");

    let flow_stem = cli
        .flow
        .file_stem()
        .expect("Invalid flow file")
        .to_string_lossy;
    let flow = get_flow_by_stem(&flow_stem);

    // Determine which node to run (by step name or default to first node)
    let node: &FlowNode = if let Some(ref step_name) = cli.step {
        flow.workflow
            .iter()
            .find(|node| node.name == *step_name)
            .unwrap_or_else(|| {
                eprintln!("Step '{}' not found in flow.", step_name);
                std::process::exit(1);
            })
    } else {
        &flow.workflow[0]
    };

    // Select steps to run based on config
    let steps_to_run = get_steps_for_tool(&node.name, &node.steps, &config);

    // Run the steps
    node.tool.invoke(steps_to_run);
}
