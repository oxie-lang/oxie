// Calls through a type, functions named like types, a trait's `new`, and a module function
// called from a method of the same name.
use std::fmt;

trait Make {
    fn new(n: i32) -> Self;
}

struct Pair {
    a: i32,
    b: i32,
}

impl Make for Pair {
    fn new(n: i32) -> Self {
        Pair { a: n, b: n * 2 }
    }
}

struct Counter {
    n: i32,
}

impl Counter {
    fn get(&self) -> i32 {
        self.n
    }

    fn twice(&self) -> i32 {
        Counter::get(self) * 2
    }

    fn label(&self) -> String {
        label(self.n)
    }
}

fn label(n: i32) -> String {
    format!("#{}", n)
}

struct Wrapper {
    inner: i32,
}

#[allow(non_snake_case)]
fn Wrapper(inner: i32) -> Wrapper {
    Wrapper { inner: inner + 1 }
}

impl fmt::Display for Wrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = format!("w{}", self.inner);
        text.fmt(f)
    }
}

mod libc {
    #[allow(non_camel_case_types)]
    pub struct timeval {
        pub tv_sec: i64,
        pub tv_usec: i64,
    }
}

fn main() {
    let p = Pair::new(3);
    let c = Counter { n: 5 };
    let t = libc::timeval { tv_sec: 1, tv_usec: 2 };
    println!("{} {} {} {} {} {} {}", p.a, p.b, c.twice(), c.label(), Wrapper(1), t.tv_sec, t.tv_usec);
}
