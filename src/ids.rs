//! Index newtypes into the vectors of an [`crate::analysis::Analysis`].
//!
//! Tests, coverage classes, regions, and files are all addressed by position.
//! Separate types keep a class index from being used as a test index.

use serde::{Deserialize, Serialize};

macro_rules! index_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl $name {
            pub fn new(index: usize) -> Self {
                Self(u32::try_from(index).expect(concat!(stringify!($name), " overflows u32")))
            }

            pub fn get(self) -> usize {
                self.0 as usize
            }
        }
    };
}

index_type!(
    /// Position in `Analysis::tests`.
    TestIdx
);
index_type!(
    /// Position in `Analysis::classes`.
    ClassIdx
);
index_type!(
    /// Position in `Analysis::regions`.
    RegionIdx
);
index_type!(
    /// Position in `Analysis::files`.
    FileIdx
);
