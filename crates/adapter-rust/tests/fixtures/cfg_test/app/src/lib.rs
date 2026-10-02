mod a;
mod b;
mod c;

pub fn entry() -> u32 {
    a::run() + c::run()
}
