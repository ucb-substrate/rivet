use crate::Step;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct RustStep<F> {
    pub work_dir: PathBuf,
    pub rust_fn: F,
    pub dependencies: Vec<Arc<dyn Step>>,
    pub pinned: bool,
}

impl<F> Debug for RustStep<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustStep")
            .field("work_dir", &self.work_dir)
            .field("rust_fn", &"<fn>")
            .field("dependencies", &self.dependencies)
            .field("pinned", &self.pinned)
            .finish()
    }
}

impl<F> RustStep<F> {
    pub fn new(work_dir: impl Into<PathBuf>, rust_fn: F, deps: Vec<Arc<dyn Step>>) -> Self {
        let dir = work_dir.into();
        RustStep {
            work_dir: dir,
            rust_fn,
            dependencies: deps,
            pinned: false,
        }
    }

    pub fn pin(&mut self) {
        self.pinned = true;
    }
}

impl<F: Fn(&PathBuf) + Send + Sync> Step for RustStep<F> {
    fn execute(&self) {
        (self.rust_fn)(&self.work_dir);
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }
}
