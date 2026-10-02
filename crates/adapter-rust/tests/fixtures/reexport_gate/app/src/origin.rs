pub(crate) fn prod() -> u32 {
    1
}

#[cfg(test)]
pub(crate) fn attr_item() -> u32 {
    2
}

#[cfg(test)]
pub(crate) fn inline_item() -> u32 {
    3
}

#[cfg(test)]
pub(crate) fn file_item() -> u32 {
    4
}
