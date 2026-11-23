use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Entries {
    pub(crate) values: Vec<Arc<String>>,
}
