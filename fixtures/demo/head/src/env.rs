//! Variable bindings for evaluation.

use std::collections::HashMap;

/// A set of variable bindings.
///
/// Pass an `Env` to [`evaluate`](crate::evaluate) to resolve
/// [`Expr::Var`](crate::ast::Expr::Var) nodes.
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: HashMap<String, f64>,
    /// Whether lookups of unknown variables fall back to zero.
    pub lenient: bool,
}

impl Env {
    /// Creates an empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `name` to `value`, replacing any earlier binding.
    pub fn set(&mut self, name: impl Into<String>, value: f64) -> &mut Self {
        self.vars.insert(name.into(), value);
        self
    }

    /// Looks up a variable.
    pub fn get(&self, name: &str) -> Option<f64> {
        self.vars.get(name).copied()
    }
}
