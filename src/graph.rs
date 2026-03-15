use std::collections::{HashMap, HashSet, VecDeque};

use crate::config::DevConfig;
use crate::error::{DevError, Result};

pub struct DependencyGraph {
    /// Services in topological start order
    order: Vec<String>,
    /// Groups of services that can start concurrently (each wave's deps are fully in prior waves)
    waves: Vec<Vec<String>>,
    /// Per-service direct deps (node → what it depends on)
    deps: HashMap<String, Vec<String>>,
    /// Reverse: per-service direct dependents (node → what depends on it)
    reverse_deps: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    pub fn from_config(cfg: &DevConfig) -> Result<Self> {
        let names: Vec<&str> = cfg.service.keys().map(|s| s.as_str()).collect();

        let mut in_degree: HashMap<&str, usize> = names.iter().map(|n| (*n, 0)).collect();
        let mut dependents: HashMap<&str, Vec<&str>> = names.iter().map(|n| (*n, vec![])).collect();
        let deps: HashMap<String, Vec<String>> = cfg
            .service
            .iter()
            .map(|(k, v)| (k.clone(), v.depends_on.clone()))
            .collect();

        // reverse_deps: node → services that directly depend on it
        let mut reverse_deps: HashMap<String, Vec<String>> =
            names.iter().map(|n| (n.to_string(), vec![])).collect();

        for (name, svc) in &cfg.service {
            for dep in &svc.depends_on {
                *in_degree.entry(name.as_str()).or_insert(0) += 1;
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(name.as_str());
                reverse_deps
                    .entry(dep.clone())
                    .or_default()
                    .push(name.clone());
            }
        }

        // BFS level-by-level: each level becomes one startup wave.
        // Preserves declaration order as tiebreaker (names vec is insertion-ordered).
        let mut queue: VecDeque<&str> = names
            .iter()
            .filter(|n| in_degree[*n] == 0)
            .copied()
            .collect();
        let mut order = Vec::with_capacity(names.len());
        let mut waves: Vec<Vec<String>> = Vec::new();

        while !queue.is_empty() {
            let wave: Vec<&str> = queue.drain(..).collect();
            let mut next: VecDeque<&str> = VecDeque::new();
            for &node in &wave {
                order.push(node.to_string());
                for &dep in &dependents[node] {
                    let deg = in_degree.get_mut(dep).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        next.push_back(dep);
                    }
                }
            }
            waves.push(wave.iter().map(|s| s.to_string()).collect());
            queue = next;
        }

        if order.len() < names.len() {
            let cycled: Vec<&str> = names
                .iter()
                .filter(|n| !order.iter().any(|o| o == *n))
                .copied()
                .collect();
            return Err(DevError::Cycle(cycled.join(", ")));
        }

        Ok(Self {
            order,
            waves,
            deps,
            reverse_deps,
        })
    }

    pub fn start_order(&self) -> &[String] {
        &self.order
    }

    /// Groups of services that can start concurrently.
    /// All services in wave N have their deps satisfied by waves 0..N.
    pub fn start_waves(&self) -> &[Vec<String>] {
        &self.waves
    }

    #[allow(dead_code)]
    pub fn stop_order(&self) -> impl Iterator<Item = &str> {
        self.order.iter().rev().map(|s| s.as_str())
    }

    /// Returns `names` plus all services that transitively depend on any of them,
    /// in reverse topological order (dependents first, named services last).
    /// Use this to determine safe stop order: stop dependents before stopping targets.
    pub fn transitive_dependents_stop_order(&self, names: &[&str]) -> Vec<String> {
        let mut needed: HashSet<String> = names.iter().map(|s| s.to_string()).collect();
        let mut queue: VecDeque<String> = names.iter().map(|s| s.to_string()).collect();
        while let Some(node) = queue.pop_front() {
            for dependent in self.reverse_deps.get(&node).into_iter().flatten() {
                if needed.insert(dependent.clone()) {
                    queue.push_back(dependent.clone());
                }
            }
        }
        // Reverse topological = stop order
        self.order
            .iter()
            .rev()
            .filter(|n| needed.contains(*n))
            .cloned()
            .collect()
    }

    /// Returns `names` plus all their transitive deps, in topological start order.
    pub fn transitive_start_order(&self, names: &[&str]) -> Vec<String> {
        let mut needed: HashSet<String> = names.iter().map(|s| s.to_string()).collect();
        let mut queue: VecDeque<String> = names.iter().map(|s| s.to_string()).collect();
        while let Some(node) = queue.pop_front() {
            for dep in self.deps.get(&node).into_iter().flatten() {
                if needed.insert(dep.clone()) {
                    queue.push_back(dep.clone());
                }
            }
        }
        self.order
            .iter()
            .filter(|n| needed.contains(*n))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DevConfig, ServiceDef};
    use indexmap::IndexMap;

    fn make_config(services: Vec<(&str, Vec<&str>)>) -> DevConfig {
        let mut map = IndexMap::new();
        for (i, (name, deps)) in services.into_iter().enumerate() {
            map.insert(
                name.to_string(),
                ServiceDef {
                    cmd: "echo".into(),
                    dir: None,
                    port: 8000 + i as u16,
                    subdomain: None,
                    env: Default::default(),
                    env_file: None,
                    log_file: None,
                    log_rotate_mb: 0,
                    pre_start: None,
                    post_stop: None,
                    depends_on: deps.iter().map(|s| s.to_string()).collect(),
                    watch: None,
                    health: None,
                    restart: Default::default(),
                    stop_timeout: std::time::Duration::from_secs(5),
                    disabled: false,
                    labels: vec![],
                    k8s: None,
                },
            );
        }
        DevConfig {
            dev: Default::default(),
            service: map,
            env_override: Default::default(),
        }
    }

    #[test]
    fn test_simple_order() {
        let cfg = make_config(vec![("b", vec!["a"]), ("a", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let order = g.start_order();
        assert!(order.iter().position(|s| s == "a") < order.iter().position(|s| s == "b"));
    }

    #[test]
    fn test_cycle_detected() {
        let cfg = make_config(vec![("a", vec!["b"]), ("b", vec!["a"])]);
        assert!(DependencyGraph::from_config(&cfg).is_err());
    }

    #[test]
    fn test_stop_order_is_reverse_of_start() {
        let cfg = make_config(vec![("b", vec!["a"]), ("a", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let start: Vec<&str> = g.start_order().iter().map(|s| s.as_str()).collect();
        let stop: Vec<&str> = g.stop_order().collect();
        assert_eq!(start, stop.iter().rev().copied().collect::<Vec<_>>());
    }

    #[test]
    fn test_no_deps_preserves_declaration_order() {
        let cfg = make_config(vec![("alpha", vec![]), ("beta", vec![]), ("gamma", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let order = g.start_order();
        assert_eq!(order, &["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_chain_three() {
        let cfg = make_config(vec![("c", vec!["b"]), ("b", vec!["a"]), ("a", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let order = g.start_order();
        let pos = |s: &str| order.iter().position(|x| x == s).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn test_empty_config() {
        let cfg = make_config(vec![]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        assert!(g.start_order().is_empty());
    }

    #[test]
    fn test_start_waves_no_deps() {
        // All independent services → single wave
        let cfg = make_config(vec![("a", vec![]), ("b", vec![]), ("c", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let waves = g.start_waves();
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].len(), 3);
    }

    #[test]
    fn test_start_waves_chain() {
        // c→b→a: three separate waves
        let cfg = make_config(vec![("c", vec!["b"]), ("b", vec!["a"]), ("a", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let waves = g.start_waves();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], ["a"]);
        assert_eq!(waves[1], ["b"]);
        assert_eq!(waves[2], ["c"]);
    }

    #[test]
    fn test_start_waves_diamond() {
        // d depends on b and c; b and c both depend on a
        // wave 0: a, wave 1: b+c, wave 2: d
        let cfg = make_config(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
            ("d", vec!["b", "c"]),
        ]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let waves = g.start_waves();
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0], ["a"]);
        // b and c in same wave (order may vary)
        assert_eq!(waves[1].len(), 2);
        assert!(waves[1].contains(&"b".to_string()));
        assert!(waves[1].contains(&"c".to_string()));
        assert_eq!(waves[2], ["d"]);
    }

    #[test]
    fn test_transitive_start_order_direct() {
        let cfg = make_config(vec![("a", vec![]), ("b", vec!["a"]), ("c", vec!["b"])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        // Requesting "c" should pull in a, b, c
        let result = g.transitive_start_order(&["c"]);
        assert_eq!(result, ["a", "b", "c"]);
    }

    #[test]
    fn test_transitive_start_order_subset() {
        // Only a and b declared, requesting "b" only needs a+b (not c if c existed)
        let cfg = make_config(vec![("a", vec![]), ("b", vec!["a"]), ("c", vec![])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let result = g.transitive_start_order(&["b"]);
        assert_eq!(result, ["a", "b"]);
        // c not included
        assert!(!result.contains(&"c".to_string()));
    }

    #[test]
    fn test_transitive_start_order_already_included() {
        let cfg = make_config(vec![("a", vec![]), ("b", vec!["a"])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let result = g.transitive_start_order(&["a", "b"]);
        assert_eq!(result, ["a", "b"]);
    }

    #[test]
    fn test_transitive_dependents_stop_order_chain() {
        // c→b→a: stopping "a" should also stop b then c first
        let cfg = make_config(vec![("a", vec![]), ("b", vec!["a"]), ("c", vec!["b"])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let result = g.transitive_dependents_stop_order(&["a"]);
        // dependents first: c, b, then a
        assert_eq!(result, ["c", "b", "a"]);
    }

    #[test]
    fn test_transitive_dependents_stop_order_leaf() {
        // stopping a leaf (no dependents) returns just itself
        let cfg = make_config(vec![("a", vec![]), ("b", vec!["a"])]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let result = g.transitive_dependents_stop_order(&["b"]);
        assert_eq!(result, ["b"]);
    }

    #[test]
    fn test_transitive_dependents_stop_order_diamond() {
        // d→b,c→a: stopping "a" should stop d, b, c, a (dependents before deps)
        let cfg = make_config(vec![
            ("a", vec![]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
            ("d", vec!["b", "c"]),
        ]);
        let g = DependencyGraph::from_config(&cfg).unwrap();
        let result = g.transitive_dependents_stop_order(&["a"]);
        // d must come before b and c; b and c before a
        let pos = |s: &str| result.iter().position(|x| x == s).unwrap();
        assert!(pos("d") < pos("b"));
        assert!(pos("d") < pos("c"));
        assert!(pos("b") < pos("a"));
        assert!(pos("c") < pos("a"));
        assert_eq!(result.len(), 4);
    }
}
