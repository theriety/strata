//! String interning with stable, insertion-order indices.
//!
//! The interner assigns each distinct name a sequential `u32` the first time it
//! is seen. Because indices follow insertion order — never hash-map iteration
//! order — no nondeterminism from the underlying [`HashMap`] can ever leak into
//! solver outputs.

use std::collections::HashMap;

use smol_str::SmolStr;

/// A string interner with stable insertion-order indices.
///
/// Interning a name returns its existing index or assigns the next sequential
/// one; the assigned indices form the dense range `0..len`, in the order names
/// were first interned.
#[derive(Debug, Clone, Default)]
pub struct Interner {
    /// Maps each interned name to its assigned index.
    map: HashMap<SmolStr, u32>,
    /// Names in assignment order; `names[i]` is the name with index `i`.
    names: Vec<SmolStr>,
}

impl Interner {
    /// Creates an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the existing index for `name`, or assigns and returns the next
    /// sequential one.
    ///
    /// Indices saturate at [`u32::MAX`], unreachable under the snapshot size
    /// budget (100k symbols).
    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.map.get(name) {
            return id;
        }

        let id = u32::try_from(self.names.len()).unwrap_or(u32::MAX);
        let name = SmolStr::new(name);
        self.names.push(name.clone());
        self.map.insert(name, id);
        id
    }

    /// Resolves an interned index back to its name, or `None` if `id` was never
    /// assigned by this interner.
    #[must_use]
    pub fn resolve(&self, id: u32) -> Option<&SmolStr> {
        self.names.get(id as usize)
    }

    /// Returns the number of distinct names interned so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Returns `true` if no names have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_assign_sequential_indices_in_insertion_order() {
        let mut interner = Interner::new();

        let ids = [
            interner.intern("alpha"),
            interner.intern("beta"),
            interner.intern("gamma"),
        ];

        assert_eq!(ids, [0, 1, 2]);
    }

    #[test]
    fn should_return_the_same_index_for_a_repeated_name() {
        let mut interner = Interner::new();

        let first = interner.intern("alpha");
        let _other = interner.intern("beta");
        let again = interner.intern("alpha");

        assert_eq!(first, again);
    }

    #[test]
    fn should_not_grow_when_re_interning_an_existing_name() {
        let mut interner = Interner::new();

        interner.intern("alpha");
        interner.intern("alpha");

        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn should_resolve_an_index_back_to_its_name() {
        let mut interner = Interner::new();

        let id = interner.intern("alpha");

        assert_eq!(interner.resolve(id), Some(&SmolStr::new("alpha")));
    }

    #[test]
    fn should_resolve_to_none_for_an_unknown_index() {
        let interner = Interner::new();

        assert_eq!(interner.resolve(7), None);
    }

    #[test]
    fn should_report_emptiness_until_a_name_is_interned() {
        let mut interner = Interner::new();
        assert!(interner.is_empty());

        interner.intern("alpha");

        assert!(!interner.is_empty());
    }
}
