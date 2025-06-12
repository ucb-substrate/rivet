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
