// A closure whose body is kept as Rust keeps its parameter's name, and a path through an
// all-caps type name (`NSURL::fileURLWithPath`) stays a path.
mod sys {
    #[allow(non_camel_case_types)]
    pub struct NSURL(pub String);
    impl NSURL {
        #[allow(non_snake_case)]
        pub fn fileURLWithPath(p: &str) -> NSURL {
            NSURL(format!("file://{p}"))
        }
    }
}

struct Label {
    text: Box<dyn Fn(&str, usize) -> String>,
    name: Option<String>,
}

struct Loader(Box<dyn Fn(&str) -> String>);

impl Label {
    fn render(&self) -> String {
        self.name.as_ref().map(|name| (self.text)(name, name.len())).unwrap_or_default()
    }

    fn load(&self, loader: &Loader) -> Option<String> {
        self.name.as_ref().map(|name| (loader.0)(name))
    }
}

fn main() {
    use sys::NSURL;
    let url = NSURL::fileURLWithPath("/tmp/x");
    println!("{}", url.0);
    let l = Label { text: Box::new(|n, k| format!("{n}:{k}")), name: Some("ab".into()) };
    println!("{}", l.render());
    println!("{:?}", l.load(&Loader(Box::new(|n| n.to_uppercase()))));
}
