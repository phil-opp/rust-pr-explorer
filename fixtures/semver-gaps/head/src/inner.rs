use std::rc::Rc;

#[derive(Default)]
pub(crate) struct Entries {
    pub(crate) values: Vec<Rc<String>>,
}
