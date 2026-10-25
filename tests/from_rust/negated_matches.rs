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

// and `matches!` as an operand of `as` and arithmetic
fn ended_count(all: &[Status]) -> u32 {
    let mut n = 0;
    for s in all {
        n = n + matches!(s, Status::Ended(_)) as u32;
    }
    n + (matches!(all.first(), Some(Status::Running)) as u32) * 10
}

fn main() {
    for s in [Status::Running, Status::Paused(2), Status::Ended("done".into())] {
        println!("{:?}: active {}, waiting {}", s, active(&s), waiting(&s));
    }
    println!("{}", ended_count(&[Status::Running, Status::Ended("a".into()), Status::Ended("b".into())]));
}
