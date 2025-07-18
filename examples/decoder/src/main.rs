use sky130::reference_flow;

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
    let args = CliArgs::parse();
    let config_str = fs::read_to_string(&args.config).expect("Failed to read config file");
    let config: Config = toml::from_str(&config_str).expect("Failed to parse config file");
    let work_dir = args.work_dir.unwrap_or("build".into());
    let flow = reference_flow(work_dir);
    f.flow.execute(args.node, &self.config);
}
