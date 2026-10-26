// A struct implementing two traits that have a method of the same name: one implementation
// joins the class, the other stays an `impl` block. A closure with typed parameters passed to a
// constructor stays a lambda.
trait Named {
    fn name(&self) -> String;
}

trait Titled {
    fn name(&self) -> String;
}

struct Book {
    title: String,
}

impl Named for Book {
    fn name(&self) -> String {
        self.title.clone()
    }
}

impl Titled for Book {
    fn name(&self) -> String {
        self.title.to_uppercase()
    }
}

struct Holder<F: Fn(u64) -> u64> {
    f: F,
}

impl<F: Fn(u64) -> u64> Holder<F> {
    fn new(f: F) -> Self {
        Holder { f }
    }
}

fn main() {
    let b = Book { title: "dune".to_string() };
    println!("{} {}", Named::name(&b), Titled::name(&b));
    let h = Holder::new(|n: u64| n * 2);
    println!("{}", (h.f)(21));
}
