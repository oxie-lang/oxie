// A module path whose module has the name of a field: `servers::spec(..)` beside `self.servers`.
mod servers {
    pub const CUSTOM: &str = "custom";
    pub fn spec(language: &str) -> Option<&'static str> {
        if language == "rust" { Some("rust-analyzer") } else { None }
    }
}

struct Lsp {
    servers: Vec<String>,
}

impl Lsp {
    fn start(&mut self, language: &str) -> usize {
        let spec = servers::spec(language).unwrap_or(servers::CUSTOM);
        self.servers.push(spec.to_string());
        self.servers.len()
    }
}

// a struct Oxie keeps as a `struct` with its `impl`
struct Store {
    servers: Vec<String>,
    #[cfg(test)]
    tested: bool,
}

impl Store {
    fn add(&mut self, language: &str) {
        let name = servers::spec(language).unwrap_or(servers::CUSTOM);
        self.servers.push(name.into());
    }
}

fn main() {
    let mut s = Store { servers: Vec::new(), #[cfg(test)] tested: false };
    s.add("rust");
    println!("{:?}", s.servers);
    let mut l = Lsp { servers: Vec::new() };
    println!("{} {}", l.start("rust"), l.start("go"));
    println!("{:?}", l.servers);
}
