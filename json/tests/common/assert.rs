//! A bounded divergence list for table-driven differentials.

#[derive(Default)]
pub(crate) struct Divergences(Vec<String>);

impl Divergences {
    /// Later mismatches are dropped once the list is full.
    pub(crate) fn push(&mut self, message: impl Into<String>) {
        if self.0.len() < 8 {
            self.0.push(message.into());
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn assert_empty(&self, what: &str) {
        assert!(self.0.is_empty(), "{what}:\n{}", self.0.join("\n"));
    }
}
