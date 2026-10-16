use crate::exec::{self, Tool};
use crate::{Step, StepRef, StepResult};
use std::ffi::OsString;
use std::fmt::Debug;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub name: String,
    pub block: String,
    pub deps: Vec<StepRef<dyn Step>>,
    pub pinned: bool,
    /// The `bash` that runs the script and its environment; see [`Tool`].
    pub tool: Tool,
}

impl BashStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        name: impl Into<String>,
        block: impl Into<String>,
        deps: Vec<StepRef<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let file = name.into();
        let module = block.into();
        BashStep {
            work_dir: dir,
            name: file,
            block: module,
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
        // substeps and templated here, rather than running a hardcoded script path.
        let mut command = self.tool.command();
        command
            .args([format!("run_{}.sh", self.name)])
            .current_dir(&self.work_dir);

        let status = exec::run_logged_in(
            &mut command,
            &self.work_dir,
            &format!("{}.{}", self.block, self.name),
        )?;

        if !status.success() {
            return Err(format!(
                "run_{}.sh exited with {status} in {}",
                self.name,
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
        format!("{} {}", self.block, self.name)
    }

    fn log_dir(&self) -> Option<PathBuf> {
        Some(self.work_dir.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// The script runs under the interpreter it was given, with the
    /// variables it was given.
    #[test]
    fn the_script_runs_in_the_given_binary_and_environment() -> StepResult {
        let dir = std::env::temp_dir().join(format!("rivet_bash_tool_{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        // Stands in for bash: says what it was run with instead of running it.
        let bash = dir.join("bash");
        fs::write(&bash, "#!/bin/sh\necho \"$SRAM_CONFIG|$*\"\n")?;
        fs::set_permissions(&bash, fs::Permissions::from_mode(0o755))?;

        let mut step = BashStep::new(&dir, "generate_sram", "top", vec![]);
        step.set_binary(&bash);
        step.set_env("SRAM_CONFIG", "sram22_64x32");
        step.execute()?;
        let out = fs::read_to_string(dir.join("top.generate_sram.out"))?;
        fs::remove_dir_all(&dir)?;
        assert_eq!(out, "sram22_64x32|run_generate_sram.sh\n");
        Ok(())
    }
}
