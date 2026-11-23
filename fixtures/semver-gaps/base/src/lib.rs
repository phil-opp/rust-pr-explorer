//! A small cache.
use std::sync::Arc;

mod inner;

/// A shared cache of values.
pub struct Cache {
    entries: inner::Entries,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Cache { entries: inner::Entries::default() }
    }
}

/// A key into the cache.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key(pub Arc<str>);
