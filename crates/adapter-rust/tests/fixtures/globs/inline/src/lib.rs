mod b;

mod outer {
    pub(crate) fn inner() {}
}

#[cfg(feature = "x")]
use crate::outer::*;
