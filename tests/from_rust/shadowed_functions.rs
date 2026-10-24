// Free functions with the names of methods: inside the methods, Rust calls the functions.
mod units {
    pub fn scale(n: u32) -> u32 {
        n * 100
    }
}

use units::scale;

fn label(n: u32) -> String {
    format!("no.{}", n)
}

fn next(n: u32) -> u32 {
    n + 10
}

fn tag(s: &str) -> String {
    s.to_uppercase()
}

struct Counter {
    n: u32,
}

impl Counter {
    fn label(&self, prefix: u32) -> String {
        format!("{} {}", prefix, label(self.n))
    }

    fn next(&mut self, step: u32) -> u32 {
        self.n = next(self.n) + step;
        self.n
    }

    fn scale(&self, by: u32) -> u32 {
        self.n * by
    }

    fn scaled(&self) -> u32 {
        scale(self.n) + self.scale(2)
    }

    fn make(n: u32) -> Counter {
        Counter { n: next(n) }
    }
}

trait Named {
    fn name(&self) -> String;

    fn tag(&self, s: &str) -> String {
        format!("<{}>", s)
    }

    fn shout(&self) -> String {
        tag(&self.name()) + &self.tag("x")
    }
}

impl Named for Counter {
    fn name(&self) -> String {
        label(self.n)
    }
}

fn main() {
    let mut c = Counter::make(1);
    println!("{}", c.label(7));
    println!("{}", c.next(1));
    println!("{}", c.scaled());
    println!("{}", c.shout());
}
