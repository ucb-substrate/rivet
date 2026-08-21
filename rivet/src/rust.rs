use crate::Step;
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Clone)]
pub struct RustStep<F> {
    pub rust_fn: F,
    pub dependencies: Vec<Arc<dyn Step>>,
    pub pinned: bool,
    pub name: Option<String>,
}

impl<F> Debug for RustStep<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustStep")
            .field("rust_fn", &"<fn>")
            .field("dependencies", &self.dependencies)
            .field("pinned", &self.pinned)
            .finish()
    }
}

impl<F> RustStep<F> {
    pub fn new(rust_fn: F, deps: Vec<Arc<dyn Step>>) -> Self {
        RustStep {
            rust_fn,
            dependencies: deps,
            pinned: false,
            name: None,
        }
    }

    /// Set the name this step is displayed under while the flow runs.
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn pin(&mut self) {
        self.pinned = true;
    }
}

impl<F: Fn() + Send + Sync> Step for RustStep<F> {
    fn execute(&self) {
        (self.rust_fn)();
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| "RustStep".to_string())
    }
}
