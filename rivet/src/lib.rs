use by_address::ByAddress;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Debug)]
pub struct Dag<F> {
    pub node: F,
    pub directed_edges: Vec<Dag<F>>,
}
pub trait NamedNode {
    fn name(&self) -> String;
}

impl<F: NamedNode> Dag<F> {
    pub fn get_mut(&mut self, target: &String) -> Option<&mut F> {
        if &self.node.name() == target {
            Some(&mut self.node)
        } else {
            for edge in &mut self.directed_edges {
                if let Some(found) = edge.get_mut(target) {
                    return Some(found);
                }
            }
            None
        }
    }
}

pub trait Step: Debug {
    fn deps(&self) -> Vec<Arc<dyn Step>>;
    fn pinned(&self) -> bool;
    fn execute(&self);
}

pub fn execute(target: Arc<dyn Step>) {
    let mut executed = HashMap::<ByAddress<Arc<dyn Step>>, Arc<dyn Step>>::new();
    execute_inner(target, &mut executed);
}

fn execute_inner(
    step: Arc<dyn Step>,
    executed: &mut HashMap<ByAddress<Arc<dyn Step>>, Arc<dyn Step>>,
) {
    if executed.contains_key(&ByAddress(step.clone())) {
        return;
    }
    for dependency in step.deps() {
        execute_inner(dependency, executed);
    }

    if step.pinned() {
        executed.insert(ByAddress(step.clone()), Arc::clone(&step));
        return;
    }

    step.execute();

    executed.insert(ByAddress(step.clone()), Arc::clone(&step));
}

pub fn hierarchical<M, F>(dag: &Dag<M>, flat_flow_gen: &impl Fn(&M, Vec<(&M, &F)>) -> F) -> Dag<F> {
    let new_edges: Vec<Dag<F>> = dag
        .directed_edges
        .iter()
        .map(|edge_dag| hierarchical(edge_dag, flat_flow_gen))
        .collect();

    let sub_blocks: Vec<(&M, &F)> = dag
        .directed_edges
        .iter()
        .zip(new_edges.iter())
        .map(|(original_dag, new_dag)| (&original_dag.node, &new_dag.node))
        .collect();

    let new_node = flat_flow_gen(&dag.node, sub_blocks);

    Dag {
        node: new_node,
        directed_edges: new_edges,
    }
}
