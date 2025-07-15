use crate::flow::{Config, Flow, FlowNode, Step, ToolConfig};
use clap::Parser;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
struct CliArgs {
    /// The name of the final flow node to execute (e.g., 'par').
    node: String,
    /// Path to the TOML configuration file.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Debug)]
pub struct Cli {
    flow: Arc<Flow>,
    args: CliArgs,
    config: Config,
}

impl Cli {
    pub fn new(flow: Flow) -> Self {
        let args = CliArgs::parse();
        let config_str = fs::read_to_string(&args.config).expect("Failed to read config file");
        let config: Config = toml::from_str(&config_str).expect("Failed to parse config file");

        Self {
            flow: Arc::new(flow),
            args,
            config,
        }
    }

    /// Runs the workflow by executing the target node and its dependencies.
    pub fn run(&self) {
        let start_node = self
            .flow
            .workflow
            .iter()
            .find(|n| n.1.name == self.args.node) //fix(ed)?
            .unwrap_or_else(|| {
                eprintln!(
                    "Error: Node '{}' not found in the defined flow.",
                    self.args.node
                );
                std::process::exit(1);
            });
        self.flow
            .execute(start_node.0, &Arc::new(self.config.clone())); //fix(ed)?
    }
}
