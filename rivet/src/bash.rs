use crate::exec::{self, Tool};
use crate::{Step, StepRef, StepResult};
use std::ffi::OsString;
use std::fmt::Debug;
use std::path::PathBuf;

/// Runs a bash script in `work_dir`, with its output in `{script}.out` and
/// `{script}.err` there, named after the script's file stem.
#[derive(Debug, Clone)]
pub struct BashStep {
    /// Where the script runs, and where its logs go.
    pub work_dir: PathBuf,
    /// What the step is called while the flow runs, e.g. `"top generate_sram"`.
    pub name: String,
    /// The script to run. A relative path is relative to `work_dir`.
    pub script: PathBuf,
    pub deps: Vec<StepRef<dyn Step>>,
    pub pinned: bool,
    /// The `bash` that runs the script and its environment; see [`Tool`].
    pub tool: Tool,
}

impl BashStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        name: impl Into<String>,
        script: impl Into<PathBuf>,
        deps: Vec<StepRef<dyn Step>>,
    ) -> Self {
        BashStep {
            work_dir: work_dir.into(),
            name: name.into(),
            script: script.into(),
            deps,
            pinned: false,
            tool: Tool::new("/bin/bash"),
        }
    }

    pub fn pin(&mut self) {
        self.pinned = true;
    }

    /// Runs the script with `binary` instead of `/bin/bash`; see
    /// [`Tool::binary`].
    pub fn set_binary(&mut self, binary: impl Into<PathBuf>) {
        self.tool.binary = binary.into();
    }

    /// Sets `key` to `value` in the script's environment; see [`Tool::env`].
    pub fn set_env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.tool.env.insert(key.into(), value.into());
    }
}

impl Step for BashStep {
    fn execute(&self) -> StepResult {
        // TODO: Make this similar to a TCL tool where a bash script is composed of several
        // substeps and templated here, rather than running a prewritten script.
        let mut command = self.tool.command();
        command.arg(&self.script).current_dir(&self.work_dir);

        let basename = self
            .script
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.name.clone());
        let status = exec::run_logged_in(&mut command, &self.work_dir, &basename)?;

        if !status.success() {
            return Err(format!(
                "{} exited with {status} in {}",
                self.script.display(),
                self.work_dir.display()
            )
            .into());
        }
        Ok(())
    }

    fn deps(&self) -> Vec<StepRef<dyn Step>> {
        self.deps.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
    }

    fn label(&self) -> String {
        self.name.clone()
    }

    fn log_dir(&self) -> Option<PathBuf> {
        Some(self.work_dir.clone())
    }

    fn browse_files(&self) -> Vec<PathBuf> {
        vec![self.work_dir.join(&self.script)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rivet_bash_{name}_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The script runs under the interpreter it was given, with the
    /// variables it was given.
    #[test]
    fn the_script_runs_in_the_given_binary_and_environment() -> StepResult {
        let dir = scratch("tool");
        // Stands in for bash: says what it was run with instead of running it.
        let bash = dir.join("bash");
        fs::write(&bash, "#!/bin/sh\necho \"$SRAM_CONFIG|$*\"\n")?;
        fs::set_permissions(&bash, fs::Permissions::from_mode(0o755))?;

        let mut step = BashStep::new(&dir, "top generate_sram", "gen.sh", vec![]);
        step.set_binary(&bash);
        step.set_env("SRAM_CONFIG", "sram22_64x32");
        step.execute()?;
        let out = fs::read_to_string(dir.join("gen.out"))?;
        fs::remove_dir_all(&dir)?;
        assert_eq!(out, "sram22_64x32|gen.sh\n");
        Ok(())
    }

    /// A relative script is found in `work_dir`, which is also where it runs,
    /// whatever directory rivet itself was started in.
    #[test]
    fn a_relative_script_is_found_and_run_in_the_work_dir() -> StepResult {
        let dir = scratch("relative");
        fs::write(dir.join("gen.sh"), "pwd -P\n")?;

        BashStep::new(&dir, "top generate_sram", "gen.sh", vec![]).execute()?;
        let out = fs::read_to_string(dir.join("gen.out"))?;
        let expected = format!("{}\n", dir.canonicalize()?.display());
        fs::remove_dir_all(&dir)?;
        assert_eq!(out, expected);
        Ok(())
    }

    /// An absolute script runs from wherever it is, still in `work_dir`.
    #[test]
    fn an_absolute_script_runs_in_the_work_dir() -> StepResult {
        let dir = scratch("absolute");
        let scripts = scratch("absolute_scripts");
        let script = scripts.join("gen.sh");
        fs::write(&script, "pwd -P\n")?;

        let step = BashStep::new(&dir, "top generate_sram", &script, vec![]);
        assert_eq!(step.browse_files(), [script]);
        step.execute()?;
        let out = fs::read_to_string(dir.join("gen.out"))?;
        let expected = format!("{}\n", dir.canonicalize()?.display());
        fs::remove_dir_all(&dir)?;
        fs::remove_dir_all(&scripts)?;
        assert_eq!(out, expected);
        Ok(())
    }
}
