// Rust forms seen less often: `unsafe extern` blocks, a type parameter's default, generic
// associated types, const generic arguments, `for<'a>` bounds, `let else`, a labeled `break` with
// a value, byte and C strings.
use std::ops::Add;

unsafe extern "C" {
    pub safe fn abs(x: i32) -> i32;
}

#[derive(Default, Debug, Clone, Copy, PartialEq)]
enum Level {
    #[default]
    Low,
    High,
}

struct Holder<'a, T = i32> {
    x: &'a T,
}

trait Container {
    type Item<'a>
    where
        Self: 'a;
    fn first<'a>(&'a self) -> Option<Self::Item<'a>>;
}

impl Container for Vec<u8> {
    type Item<'a> = &'a u8;
    fn first<'a>(&'a self) -> Option<&'a u8> {
        self.iter().next()
    }
}

fn arr<const N: usize>() -> [u8; N] {
    [1; N]
}

fn sum_all<T>(xs: &[T]) -> T
where
    for<'a> &'a T: Add<&'a T, Output = T>,
    T: Default + Copy,
{
    let mut acc = T::default();
    for x in xs {
        acc = &acc + x;
    }
    acc
}

fn evens() -> impl Iterator<Item = u8> {
    (0..10u8).filter(|x| x % 2 == 0)
}

fn first_word(s: &str) -> Option<&str> {
    let w = s.split(' ').next()?;
    Some(w)
}

static mut COUNT: i32 = 0;

pub(in crate) fn bump() {
    unsafe {
        COUNT += 1;
    }
}

fn main() {
    let add = |a: i32| move |b: i32| a + b;
    let v = 'outer: loop {
        break 'outer 5;
    };
    let pair = ((1, 2), 3);
    let Some(w) = first_word("hello world") else { return };
    let bytes = (b'a', br"r\n", c"cstr");
    let x: [u8; { 2 + 2 }] = [0; 4];
    let h = Holder { x: &7 };
    let d: &dyn Fn(&str) -> usize = &|s: &str| s.len();
    bump();
    println!(
        "{} {} {} {} {} {:?} {:?} {:?} {} {} {} {:?} {:?} {}",
        abs(-3),
        add(1)(2),
        v,
        pair.0 .1,
        w,
        bytes.0,
        x,
        arr::<{ 1 + 2 }>(),
        h.x,
        d("abc"),
        sum_all(&[1, 2, 3]),
        Level::default(),
        vec![9u8, 8].first(),
        evens().count() + unsafe { COUNT } as usize
    );
    let _ = (bytes.1, bytes.2, Level::High, matches!(v, 1..=5 | 10));
}
