pub(crate) use crate::origin::prod;

#[cfg(test)]
pub(crate) use crate::origin::attr_item;

#[cfg(test)]
mod inline {
    pub(crate) use crate::origin::inline_item;
}

pub(crate) fn ready() -> u32 {
    prod()
}
