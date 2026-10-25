// A block expression where a variable or method named `run` would hide Oxie's `run { }`.
struct Task {
    steps: Vec<u32>,
}

impl Task {
    fn run(&self) -> u32 {
        self.steps.iter().sum()
    }

    fn report(&self) -> String {
        let total = {
            let doubled = self.run() * 2;
            doubled + 1
        };
        format!("total {total}")
    }
}

fn start(run: Option<u32>) -> u32 {
    let n = {
        let base = run.unwrap_or(0);
        base * 10
    };
    n + 1
}

fn main() {
    println!("{}", Task { steps: vec![1, 2, 3] }.report());
    println!("{} {}", start(Some(4)), start(None));
}
