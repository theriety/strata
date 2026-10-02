mod facade;
mod leak;
mod origin;

#[cfg(test)]
mod support;

pub fn entry() -> u32 {
    origin::prod() + facade::ready()
}
