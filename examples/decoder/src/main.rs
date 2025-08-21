use clap::Parser;
use rivet::flow::{Config, ToolConfig};
use sky130::reference_flow;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use toml;

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

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn test_read_from_checkpt() {
        use super::*;
        use rivet::flow::ToolStart;
        let mut tools = HashMap::new();
        let conf_start = ToolStart {
            step: "syn_map".to_string(),
            checkpoint: Some(PathBuf::from("checkpoints/syn_map.cpf")),
        };
        let conf = ToolConfig {
            start: Some(conf_start),
            stop: Some("write_design".to_string()),
            pin: None,
        };
        tools.insert("Genus".to_string(), conf.clone());
        let config = Config {
            tools: tools.clone(),
        };
        //fix hardcoding the pathbuf of the reference flow
        let flow = reference_flow(PathBuf::from(
            "/scratch/cs199-cby/rivet/examples/decoder/src",
        ));
        flow.execute("syn", &config);
        assert_eq!(2 + 2, 4);
    }
}

fn main() {
    // let args = CliArgs::parse();
    // let config_str = fs::read_to_string(&args.config).expect("Failed to read config file");
    // let config: Config = toml::from_str(&config_str).expect("Failed to parse config file");
    // let work_dir = args.work_dir.unwrap_or("build".into());

    // let mut tools = HashMap::new();
    // let conf =  ToolConfig {start: None, stop: Some("write_design".to_string()), pin: None};
    // tools.insert("Genus".to_string(), conf.clone());
    // let config = Config {tools : tools.clone() };
    // //fix hardcoding the pathbuf of the reference flow
    // let flow = reference_flow(PathBuf::from("/scratch/cs199-cby/rivet/examples/decoder/src"));
    // flow.execute("syn", &config);

    use rivet::flow::ToolStart;
    let mut tools = HashMap::new();
    let conf_start = ToolStart {
        step: "syn_map".to_string(),
        checkpoint: Some(PathBuf::from("checkpoints/syn_map.cpf")),
    };
    let conf = ToolConfig {
        start: Some(conf_start),
        stop: Some("write_design".to_string()),
        pin: None,
    };
    tools.insert("syn".to_string(), conf.clone());
    let config = Config {
        tools: tools.clone(),
    };
    //fix hardcoding the pathbuf of the reference flow
    let flow = reference_flow(
        PathBuf::from("/home/ff/eecs251b/"),
        PathBuf::from("/scratch/cs199-cby/rivet/examples/decoder/src"),
        "decoder",
    );
    flow.execute("par", &config);
}
