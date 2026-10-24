// `!matches!(..)`: the negation covers the whole test, not its subject.
#[derive(Debug)]
enum Status {
    Running,
    Paused(u32),
    Ended(String),
}

fn active(s: &Status) -> bool {
    !matches!(s, Status::Ended(_))
}

fn waiting(s: &Status) -> bool {
    !matches!(*s, Status::Running) && !matches!(s, Status::Ended(m) if m.is_empty())
}

fn main() {
    for s in [Status::Running, Status::Paused(2), Status::Ended("done".into())] {
        println!("{:?}: active {}, waiting {}", s, active(&s), waiting(&s));
    }
}
