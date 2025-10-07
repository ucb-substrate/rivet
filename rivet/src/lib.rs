use by_address::ByAddress;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
pub struct Dag<F> {
    node: F,
    directed_edges: Vec<F>,
}

pub trait Step {
    fn deps(&self) -> Vec<Arc<dyn Step>>;
    fn pinned(&self) -> bool;
    fn execute(&self);
}

pub fn execute(target: Arc<dyn Step>) {
    //traverse dag, execute deps unless they are pinned

    let mut executed = HashMap::<ByAddress<Arc<dyn Step>>, Arc<dyn Step>>::new();
    execute_inner(target, &mut executed); // this is assuming that a target is a Step not a tree of
                                          // flat flows for now
}

fn execute_inner(
    step: Arc<dyn Step>,
    executed: &mut HashMap<ByAddress<Arc<dyn Step>>, Arc<dyn Step>>,
) {
    if executed.contains_key(ByAddress(step)) {
        return;
    }
    for dependency in step.deps() {
        execute_inner(dependency, executed);
    }

    if step.pinned() {
        executed.insert(ByAddress(step), Arc::clone(step));
        return;
    }

    step.execute();

    executed.insert(ByAddress(step), Arc::clone(step));
}

pub fn hierarchical<M, F>(dag: Dag<M>, flat_flow_gen: impl Fn(&M) -> F) -> Dag<F> {
    // This is supposed to convert a dag of `ModuleInfo` and `FlatFlow` into a dag of flat flows
    let new_node = flat_flow_gen(&dag.node);

    let new_edges = dag.directed_edges.iter().map(flat_flow_gen).collect();

    Dag {
        node: new_node,
        directed_edges: new_edges,
    }
}
