// Everyday Rust: an enum with a lifetime and guards, trait objects, a builder method, shared
// state in a closure, iterator chains.
use std::collections::HashMap;
use std::fmt::{self, Display};
use std::rc::Rc;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Token<'a> {
    Word(&'a str),
    Num(i64),
    Sym { ch: char, width: usize },
}

impl<'a> Display for Token<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Word(w) => write!(f, "w:{w}"),
            Token::Num(n) if *n < 0 => write!(f, "neg:{}", -n),
            Token::Num(n) => write!(f, "n:{n:>3}"),
            Token::Sym { ch, width: w @ 1..=2 } => write!(f, "s:{ch}{w}"),
            Token::Sym { ch, .. } => write!(f, "S:{ch}"),
        }
    }
}

fn lex(s: &str) -> Vec<Token<'_>> {
    s.split_whitespace()
        .map(|w| match w.parse::<i64>() {
            Ok(n) => Token::Num(n),
            Err(_) if w.len() == 1 && !w.chars().all(char::is_alphanumeric) => Token::Sym { ch: w.chars().next().unwrap(), width: 1 },
            Err(_) => Token::Word(w),
        })
        .collect()
}

trait Shape {
    fn area(&self) -> f64;
    fn name(&self) -> String {
        format!("shape({:.1})", self.area())
    }
}

struct Sq(f64);
impl Shape for Sq {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}

#[derive(Default)]
struct Graph {
    edges: HashMap<u32, Vec<u32>>,
}

impl Graph {
    fn add(&mut self, a: u32, b: u32) -> &mut Self {
        self.edges.entry(a).or_default().push(b);
        self.edges.entry(b).or_insert_with(Vec::new);
        self
    }

    fn reach(&self, from: u32) -> Vec<u32> {
        let mut seen = vec![from];
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            for &m in self.edges.get(&n).map(|v| v.as_slice()).unwrap_or(&[]) {
                if !seen.contains(&m) {
                    seen.push(m);
                    stack.push(m);
                }
            }
        }
        seen.sort_unstable();
        seen
    }
}

fn shared() -> usize {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let l2 = Rc::clone(&log);
    let push = move |s: &str| l2.borrow_mut().push(s.to_owned());
    push("a");
    push("b");
    let n = log.borrow().len();
    n
}

fn main() {
    let toks = lex("let x = -42 + 7 ;");
    let shown: Vec<String> = toks.iter().map(ToString::to_string).collect();
    println!("{}", shown.join(" "));
    let mut sorted = toks.clone();
    sorted.sort();
    println!("{:?}", sorted.first());
    let shapes: Vec<Box<dyn Shape>> = vec![Box::new(Sq(2.0)), Box::new(Sq(1.5))];
    let total: f64 = shapes.iter().map(|s| s.area()).sum();
    println!("{} {total:.2}", shapes[1].name());
    let mut g = Graph::default();
    g.add(1, 2).add(2, 3).add(4, 1);
    println!("{:?} {:?}", g.reach(1), g.reach(4));
    println!("{}", shared());
    let words = ["apple", "bob", "kayak", "rust"];
    let pal: Vec<_> = words.iter().copied().filter(|w| w.chars().rev().collect::<String>() == *w).collect();
    let lens: HashMap<_, _> = words.iter().map(|w| (*w, w.len())).collect();
    let mut keys: Vec<_> = lens.keys().collect();
    keys.sort();
    println!("{pal:?} {keys:?} {}", lens["kayak"]);
    let nested = vec![vec![1, 2], vec![3], vec![]];
    let flat: Vec<i32> = nested.into_iter().flatten().collect();
    let (evens, odds): (Vec<i32>, Vec<i32>) = flat.iter().partition(|&&x| x % 2 == 0);
    println!("{evens:?} {odds:?} {}", flat.windows(2).filter(|w| w[0] < w[1]).count());
}
