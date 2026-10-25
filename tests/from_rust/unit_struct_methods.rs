// A method called on a unit struct value: `Quit.name()`, not the static `Quit::name()`.
#[derive(Debug, Clone, Copy)]
struct Quit;

impl Quit {
    fn name(&self) -> &'static str {
        "quit"
    }

    fn named(name: &str) -> String {
        format!("{name}!")
    }
}

fn main() {
    let names = [Quit.name(), Quit.clone().name()];
    println!("{:?} {}", names, Quit::named("go"));
}
