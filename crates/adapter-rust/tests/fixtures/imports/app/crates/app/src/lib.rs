mod alpha;
pub mod beta;
#[path = "custom/gamma_impl.rs"]
mod gamma;
mod omitted;
mod inline_host {
    pub mod nested;
}

use crate::alpha::Alpha;
use crate::alpha::helper::{Assist, assist};
use crate::{beta::beta_fn, gamma::gamma_fn};
use self::inline_host::nested::nested_fn;
use crate::beta::Reexported;
use crate::omitted::Omitted;
use imports_dep::shared::Shared;
use std::collections::HashMap;
