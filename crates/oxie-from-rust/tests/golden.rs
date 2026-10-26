//! The exact Oxie text produced for small inputs, pinning down the output style.

fn check(rust: &str, oxie: &str) {
    let t = oxie_from_rust::translate(rust).unwrap();
    assert_eq!(t.oxie, oxie, "\n--- got\n{}\n--- expected\n{}", t.oxie, oxie);
}

/// The examples in docs/tooling.md ("Translating Rust") show the real output.
#[test]
fn tooling_doc_examples_are_current() {
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/tooling.md")).unwrap();
    let mut section = doc.split("### Translating Rust").nth(1).unwrap().split("\n## ").next().unwrap();
    // each ```rust block and the ```oxie block after it
    let mut block = |lang: &str| -> Option<String> {
        let start = section.find(&format!("```{}\n", lang))? + lang.len() + 4;
        let end = start + section[start..].find("```").unwrap();
        let text = section[start..end].to_string();
        section = &section[end..];
        Some(text)
    };
    let mut pairs = 0;
    while let (Some(rust), Some(oxie)) = (block("rust"), block("oxie")) {
        check(&rust, &oxie);
        pairs += 1;
    }
    assert_eq!(pairs, 2);
}

#[test]
fn variables_and_functions() {
    check(
        r#"
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn main() {
    let x = add(1, 2);
    let mut total = 0;
    for i in 0..x {
        total += i;
    }
    println!("total = {}", total);
}
"#,
        r#"fun add(a: i32, b: i32): i32 = a + b

fun main() {
    val x = add(1, 2)
    var total = 0
    for (i in 0..x) {
        total += i
    }
    println("total = $total")
}
"#,
    );
}

#[test]
fn struct_with_new_becomes_data_class() {
    check(
        r#"
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    fn dist(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    fn shift(&mut self, dx: f64) {
        self.x += dx;
    }
}

fn main() {
    let mut p = Point::new(3.0, 4.0);
    p.shift(1.0);
    println!("{:?} {:.1}", p, p.dist());
}
"#,
        r#"data class Point(x: f64, y: f64) {
    fun dist(): f64 = (x * x + y * y).sqrt()

    fun shift(dx: f64) {
        x += dx
    }
}

fun main() {
    var p = Point(3.0, 4.0)
    p.shift(1.0)
    println("${p:?} ${p.dist():.1}")
}
"#,
    );
}

#[test]
fn other_derives_stay_explicit() {
    check(
        r#"
#[derive(Debug, Clone)]
struct User {
    name: String,
    age: u32,
}

impl User {
    fn greet(&self) -> String {
        format!("hi {}", self.name)
    }
}

fn main() {
    let u = User { name: String::from("ada"), age: 36 };
    println!("{}", u.greet());
}
"#,
        r#"@derive(Debug, Clone)
class User(name: String, age: u32) {
    fun greet(): String = "hi $name"
}

fun main() {
    val u = User { name: "ada", age: 36 }
    println(u.greet())
}
"#,
    );
}

#[test]
fn enums_match_and_display() {
    check(
        r#"
use std::fmt;

enum Shape {
    Circle { radius: f64 },
    Square(f64),
    Empty,
}

impl Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle { radius } => 3.14 * radius * radius,
            Shape::Square(s) => s * s,
            Shape::Empty => 0.0,
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Shape::Circle { radius } => write!(f, "circle({})", radius),
            Shape::Square(s) => write!(f, "square({s})"),
            Shape::Empty => write!(f, "empty"),
        }
    }
}
"#,
        r#"@no_auto_derive
enum Shape {
    Circle(radius: f64)
    Square(f64)
    Empty

    fun area(): f64 = when (self) {
        Shape.Circle(radius) -> 3.14 * radius * radius
        Shape.Square(s) -> s * s
        Shape.Empty -> 0.0
    }

    override fun toString() = when (self) {
        Shape.Circle(radius) -> "circle($radius)"
        Shape.Square(s) -> "square($s)"
        Shape.Empty -> "empty"
    }
}
"#,
    );
}

#[test]
fn results_become_throws() {
    check(
        r#"
use std::error::Error;

fn parse_port(s: &str) -> Result<u16, Box<dyn Error>> {
    let n: u16 = s.trim().parse()?;
    if n == 0 {
        return Err("port 0 is reserved".into());
    }
    Ok(n)
}

fn check(v: i32) -> Result<(), String> {
    if v < 0 {
        Err(format!("{} is negative", v))
    } else {
        Ok(())
    }
}

fn main() {
    match parse_port("80") {
        Ok(p) => println!("port {}", p),
        Err(e) => println!("error: {}", e),
    }
    if let Err(e) = check(-1) {
        println!("{e}");
    }
}
"#,
        r#"import std::error::Error

fun parse_port(s: &str): u16 throws {
    val n: u16 = s.trim().parse()?
    if (n == 0) throw "port 0 is reserved"
    n
}

fun check(v: i32) throws String {
    if (v < 0) throw "$v is negative"
}

fun main() {
    when (parse_port("80")) {
        Ok(p) -> println("port $p")
        Err(e) -> println("error: $e")
    }
    if (val Err(e) = check(-1)) println(e)
}
"#,
    );
}

#[test]
fn options_and_closures() {
    check(
        r#"
fn first_even(v: &[i32]) -> Option<i32> {
    let x = v.iter().find(|n| *n % 2 == 0)?;
    Some(*x * 10)
}

fn main() {
    let v = vec![1, 3, 4, 6];
    let doubled: Vec<i32> = v.iter().map(|x| x * 2).collect();
    let sum = v.iter().fold(0, |acc, x| acc + x);
    let none: Option<i32> = None;
    if let Some(e) = first_even(&v) {
        println!("{} {:?} {} {:?}", e, doubled, sum, none);
    }
    let mut it = v.into_iter();
    while let Some(x) = it.next() {
        print!("{x} ");
    }
}
"#,
        r#"fun first_even(v: &[i32]): i32? {
    val x = v.iter().find { *it % 2 == 0 }?
    Some(*x * 10)
}

fun main() {
    val v = vec[1, 3, 4, 6]
    val doubled: [i32] = v.iter().map { it * 2 }.collect()
    val sum = v.iter().fold(0) { acc, x -> acc + x }
    val none: i32? = null
    if (val e = first_even(&v)) println("$e ${doubled:?} $sum ${none:?}")
    var it = v.into_iter()
    while (val x = it.next()) {
        print("$x ")
    }
}
"#,
    );
}

#[test]
fn traits_become_interfaces() {
    check(
        r#"
trait Animal {
    fn name(&self) -> String;
    fn speak(&self) -> String {
        format!("{} speaks", self.name())
    }
}

struct Dog {
    age: u8,
}

impl Animal for Dog {
    fn name(&self) -> String {
        "dog".to_string()
    }
}

fn main() {
    let pets: Vec<Box<dyn Animal>> = vec![Box::new(Dog { age: 3 })];
    for p in &pets {
        println!("{}", p.speak());
    }
}
"#,
        r#"interface Animal {
    fun name(): String
    fun speak(): String = "${self.name()} speaks"
}

class Dog(age: u8) : Animal {
    override fun name(): String = "dog"
}

fun main() {
    val pets: [Box<dyn Animal>] = [Box.new(Dog { age: 3 })]
    for (p in &pets) {
        println(p.speak())
    }
}
"#,
    );
}

#[test]
fn receivers_oxie_cannot_infer_stay_explicit() {
    check(
        r#"
struct Counter {
    n: u32,
}

impl Counter {
    fn helper(x: u32) -> u32 {
        x + 1
    }

    fn into_inner(self) -> u32 {
        self.n
    }

    fn peek(&mut self) -> u32 {
        self.n
    }
}
"#,
        r#"class Counter(n: u32) {
    static fun helper(x: u32): u32 = x + 1

    fun into_inner(self): u32 = n

    fun peek(&mut self): u32 = n
}
"#,
    );
}

#[test]
fn comments_are_kept() {
    check(
        r#"
// Entry point.
fn main() {
    // say hello
    let name = "world"; // the name
    println!("hello {}", name);
}
"#,
        r#"// Entry point.
fun main() {
    // say hello
    val name = "world" // the name
    println("hello $name")
}
"#,
    );
}

#[test]
fn statement_attributes_are_kept() {
    check(
        r#"
fn main() {
    let mut v: Vec<i32> = Vec::new();
    #[cfg(test)]
    v.push(1);
    #[allow(unused_must_use)]
    "5".parse::<i32>();
    #[allow(unused_mut)]
    let mut n = v.len();
    /// not documentation
    let m = n + 1;
    if m > 0 {
        #[cfg(test)]
        println!("{}", m);
    }
    let mut add = |x: usize| {
        /// still a statement
        v.push(x as i32);
    };
    add(m);
    println!("{:?} {} {}", v, n, m);
}
"#,
        r#"fun main() {
    var v: [i32] = []
    @cfg(test) v.push(1)
    @allow(unused_must_use) "5".parse::<i32>()
    @allow(unused_mut) var n = v.len()
    // not documentation
    val m = n + 1
    if (m > 0) {
        @cfg(test) println(m)
    }
    var add = { x: usize ->
        // still a statement
        v.push(x as i32);
    }
    add(m)
    println("${v:?} $n $m")
}
"#,
    );
}

#[test]
fn attributes_on_values_elements_and_fields() {
    check(
        r#"
struct P {
    x: i32,
}

fn mode() -> &'static str {
    #[cfg(test)]
    return "test";
    #[cfg(not(test))]
    "normal"
}

fn main() {
    let v = vec![1, #[cfg(test)] 2, 3];
    let p = P { #[cfg(test)] x: 1, #[cfg(not(test))] x: 2 };
    println!("{} {:?} {}", mode(), v, p.x);
}
"#,
        r#"class P(x: i32)

fun mode(): &'static str {
    @cfg(test) return "test"
    @cfg(not(test)) "normal"
}

fun main() {
    val v = vec[1, @cfg(test) 2, 3]
    val p = P { @cfg(test) x: 1, @cfg(not(test)) x: 2 }
    println("${mode()} ${v:?} ${p.x}")
}
"#,
    );
}

/// A field holding a function is called in parentheses, as in Rust.
#[test]
fn function_fields_are_called_in_parentheses() {
    check(
        r#"
struct Button {
    on_click: Box<dyn Fn() -> i32>,
}

impl Button {
    fn click(&self) -> i32 {
        let v = (self.on_click)();
        v + 1
    }
}

fn click_twice(b: &Button) -> i32 {
    (b.on_click)() * 2
}
"#,
        r#"class Button(on_click: Box<dyn Fn() -> i32>) {
    fun click(): i32 {
        val v = on_click()
        v + 1
    }
}

fun click_twice(b: &Button): i32 = (b.on_click)() * 2
"#,
    );
}

/// Nightly-only syntax (a trait alias, a `try` block) has no Oxie form.
#[test]
fn untranslatable_code_stays_rust_with_a_todo() {
    check(
        r#"
#![feature(try_blocks, trait_alias)]

trait Number = Copy + std::ops::Add<Output = Self>;

fn double<T: Number>(x: T) -> T {
    x + x
}

fn parse_sum(a: &str, b: &str) -> Option<i32> {
    let sum: Option<i32> = try { a.parse::<i32>().ok()? + b.parse::<i32>().ok()? };
    sum
}
"#,
        r#"#![feature(try_blocks, trait_alias)]

// TODO(from-rust): kept as Rust: no Oxie syntax for this item
rust {
    trait Number = Copy + std::ops::Add<Output = Self>;
}

fun double<T: Number>(x: T): T = x + x

fun parse_sum(a: &str, b: &str): i32? {
    // TODO(from-rust): kept as Rust: no Oxie syntax for this expression
    val sum: i32? = rust { try { a.parse::<i32>().ok()? + b.parse::<i32>().ok()? } }
    sum
}
"#,
    );
}

/// `extern` blocks, `extern crate` and raw pointers are written as in Rust.
#[test]
fn extern_blocks_and_raw_pointers() {
    check(
        r#"
extern crate alloc;

extern "C" {
    fn abs(x: i32) -> i32;
}

fn main() {
    let mut x = unsafe { abs(-3) };
    let p = &raw mut x;
    unsafe { *p += 1 }
    println!("{}", x);
}
"#,
        r#"extern crate alloc

extern "C" {
    fn abs(x: i32) -> i32;
}

fun main() {
    var x = unsafe { abs(-3) }
    val p = &raw mut x
    unsafe { *p += 1 }
    println(x)
}
"#,
    );
}

/// A method Oxie rejects is kept as Rust in an `impl` block of its own; the class keeps the rest.
/// (Rejected here: a `box` pattern, which only nightly Rust has.)
#[test]
fn rejected_method_stays_rust_alone() {
    check(
        r#"
#![feature(box_patterns)]

struct Counter {
    n: i32,
}

impl Counter {
    fn next(&mut self) -> i32 {
        self.n += 1;
        self.n
    }

    // reads a boxed value
    fn unbox(&self, b: Box<i32>) -> i32 {
        match b {
            box v => v + self.n,
        }
    }
}

fn main() {
    let mut c = Counter { n: 2 };
    println!("{} {}", c.next(), c.unbox(Box::new(4)));
}
"#,
        r#"#![feature(box_patterns)]

class Counter(n: i32) {
    fun next(): i32 {
        n += 1
        n
    }
}

// reads a boxed value
// TODO(from-rust): kept as Rust: Oxie rejected the translation (expected `->`, found `v`)
rust {
    impl Counter { fn unbox(&self, b : Box<i32>) -> i32 { match b { box v => v + self.n, } } }
}

fun main() {
    var c = Counter { n: 2 }
    println("${c.next()} ${c.unbox(Box.new(4))}")
}
"#,
    );
}

/// The same for an `impl` of a type that isn't a class (here a tuple struct).
#[test]
fn rejected_method_of_an_impl_stays_rust_alone() {
    check(
        r#"
#![feature(box_patterns)]

struct Meters(i32);

impl Meters {
    fn get(&self) -> i32 {
        self.0
    }

    fn unbox(&self, b: Box<i32>) -> i32 {
        match b {
            box v => v + self.0,
        }
    }
}

fn main() {
    let m = Meters(5);
    println!("{} {}", m.get(), m.unbox(Box::new(1)));
}
"#,
        r#"#![feature(box_patterns)]

struct Meters(i32)

impl Meters {
    fun get(): i32 = self.0
}

// TODO(from-rust): kept as Rust: Oxie rejected the translation (expected `->`, found `v`)
rust {
    impl Meters { fn unbox(&self, b : Box<i32>) -> i32 { match b { box v => v + self.0, } } }
}

fun main() {
    val m = Meters(5)
    println("${m.get()} ${m.unbox(Box.new(1))}")
}
"#,
    );
}

/// A let chain whose first condition ends in a lambda.
#[test]
fn let_chain_with_a_lambda() {
    check(
        r#"
fn first(v: &[i32]) -> Option<i32> {
    v.first().copied()
}

fn both(v: &[i32]) -> i32 {
    if let Some(a) = first(v).filter(|x| *x > 0) && let Some(b) = first(&v[1..]) {
        a + b
    } else {
        0
    }
}

fn main() {
    println!("{} {}", both(&[1, 2]), both(&[-1, 2]));
}
"#,
        r#"fun first(v: &[i32]): i32? = v.first().copied()

fun both(v: &[i32]): i32 {
    if (val a = first(v).filter { *it > 0 } && val b = first(&v[1..])) a + b else 0
}

fun main() {
    println("${both(&[1, 2])} ${both(&[-1, 2])}")
}
"#,
    );
}

/// In a method, Oxie calls the type's own function for a bare `helper(..)`, so a free function
/// of that name is called through its module.
#[test]
fn free_function_hidden_by_a_method() {
    check(
        r#"
fn helper(n: usize) -> usize {
    n * 10
}

struct Counter {
    n: usize,
}

impl Counter {
    fn helper(&self, k: usize) -> usize {
        self.n + k
    }

    fn scaled(&self) -> usize {
        helper(self.n)
    }
}

fn main() {
    let c = Counter { n: 2 };
    println!("{} {}", c.helper(1), c.scaled());
}
"#,
        r#"fun helper(n: usize): usize = n * 10

class Counter(n: usize) {
    fun helper(k: usize): usize = n + k

    fun scaled(): usize = self::helper(n)
}

fun main() {
    val c = Counter { n: 2 }
    println("${c.helper(1)} ${c.scaled()}")
}
"#,
    );
}

/// GPUI-style code: `px(8.)` floats, `a?..b` ranges, and `None` beside an `Option` Oxie can't see.
#[test]
fn rust_literals_ranges_and_none_branches() {
    check(
        r#"
use std::ops::Range;

fn px(v: f32) -> f32 {
    v * 2.
}

fn find(v: &[usize], x: usize) -> Option<usize> {
    v.iter().position(|y| *y == x)
}

fn shift(r: Range<usize>, d: isize) -> Option<Range<usize>> {
    Some(r.start.checked_add_signed(d)?..r.end.checked_add_signed(d)?)
}

fn main() {
    let v = vec![1, 2, 3];
    let found = if v.len() > 1 { find(&v, 2) } else { None };
    let home = if v.len() > 5 { std::env::var_os("OXIE_UNSET") } else { None };
    println!("{} {:?} {:?} {:?}", px(8.), shift(1..3, 1), found, home);
}
"#,
        r#"import std::ops::Range

fun px(v: f32): f32 = v * 2.

fun find(v: &[usize], x: usize): usize? = v.iter().position { *it == x }

fun shift(r: Range<usize>, d: isize): Range<usize>? {
    Some(r.start.checked_add_signed(d)?..r.end.checked_add_signed(d)?)
}

fun main() {
    val v = vec[1, 2, 3]
    val found = if (v.len() > 1) find(&v, 2) else null
    val home = if (v.len() > 5) std.env.var_os("OXIE_UNSET") else None
    println("${px(8.)} ${shift(1..3, 1):?} ${found:?} ${home:?}")
}
"#,
    );
}

#[test]
fn context_parameters_of_a_class() {
    check(
        r#"
pub struct Editor {
    text: String,
}

impl Editor {
    pub fn insert(&mut self, s: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.text.push_str(s);
        self.refresh(window, cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.notify(cx);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.invalidate();
        self.notify(cx);
    }

    fn notify(&self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn len(&self) -> usize {
        self.text.len()
    }

    fn on_key(&mut self, key: &str, _window: &mut Window, cx: &mut Context<Self>) {
        cx.defer(|this, cx| this.clear(cx));
        let n = self.len();
        Self::log(n, cx);
    }

    fn log(n: usize, cx: &mut Context<Self>) {
        cx.note(n);
    }
}
"#,
        r#"context(window: &mut Window, cx: &mut Context<Self>)
pub class Editor(text: String) {
    pub fun insert(s: &str) {
        text.push_str(s)
        refresh()
    }

    pub fun clear() {
        text.clear()
        notify()
    }

    fun refresh(&mut self) {
        window.invalidate()
        notify()
    }

    fun notify() {
        cx.notify()
    }

    fun len(): usize = text.len()

    fun on_key(&mut self, key: &str, _window: &mut Window, cx: &mut Context<Self>) {
        cx.defer { r#this, cx -> r#this.clear(cx) }
        val n = len()
        Self.log(n)
    }

    static fun log(n: usize) {
        cx.note(n)
    }
}
"#,
    );
}

#[test]
fn context_parameters_across_files() {
    let workspace = r#"
mod saving;

pub struct Workspace {
    tabs: Vec<String>,
}

impl Workspace {
    pub fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(path.to_string());
        self.save(window, cx);
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.pop();
        self.save(window, cx);
    }
}
"#;
    let saving = r#"
use super::*;

impl Workspace {
    pub(super) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_title("saved");
        self.saved(cx);
    }

    fn saved(&self, cx: &mut Context<Self>) {
        cx.notify();
    }
}
"#;
    let out: Vec<String> = oxie_from_rust::translate_crate(&[workspace, saving]).into_iter().map(|t| t.unwrap().oxie).collect();
    assert_eq!(
        out[0],
        r#"// TODO(from-rust): remove this line if saving.rs is translated to saving.oxi too
mod saving

context(window: &mut Window, cx: &mut Context<Self>)
pub class Workspace(tabs: [String]) {
    pub fun open(path: &str) {
        tabs.push(path.to_string())
        self.save()
    }

    pub fun close() {
        tabs.pop()
        self.save()
    }
}
"#
    );
    assert_eq!(
        out[1],
        r#"import super::*;

impl Workspace {
    pub(super) fun save(&mut self) {
        window.set_title("saved")
        self.saved()
    }

    fun saved() {
        cx.notify()
    }
}
"#
    );
}

#[test]
fn public_method_takes_the_contexts_it_uses() {
    check(
        r#"
pub struct Pane {
    items: Vec<String>,
}

impl Pane {
    pub fn add(&mut self, item: String, cx: &mut Context<Self>) {
        self.items.push(item);
        self.changed(cx);
    }

    pub fn remove(&mut self, cx: &mut Context<Self>) {
        self.items.pop();
        self.changed(cx);
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.notify();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}
"#,
        r#"context(cx: &mut Context<Self>)
pub class Pane(items: [String]) {
    pub fun add(item: String) {
        items.push(item)
        changed()
    }

    pub fun remove() {
        items.pop()
        changed()
    }

    fun changed() {
        cx.notify()
    }

    pub fun len(): usize = items.len()
}
"#,
    );
}

#[test]
fn returns_inside_values() {
    // a `return` in a branch whose value is used returns from the function, in Oxie too
    check(
        r#"
fn name(n: i32) -> Option<String> {
    Some(match n {
        1 => "one",
        _ => return None,
    }.to_string())
}
"#,
        r#"fun name(n: i32): String? {
    Some((when (n) {
        1 -> "one"
        else -> return null
    }).to_string())
}
"#,
    );
}

#[test]
fn module_paths_beside_fields_of_another_file() {
    // the struct is in the first file and its method in the second: `servers::COPILOT` stays a
    // path, since `servers.COPILOT` would read the field
    let main = r#"
mod servers;
mod copilot;

pub struct Store {
    servers: Vec<String>,
    #[cfg(test)]
    tested: bool,
}
"#;
    let copilot = r#"
use crate::{servers, Store};

impl Store {
    pub fn sign_in(&mut self) {
        self.servers.push(servers::COPILOT.to_string());
    }
}
"#;
    let out: Vec<String> = oxie_from_rust::translate_crate(&[main, copilot]).into_iter().map(|t| t.unwrap().oxie).collect();
    assert!(out[1].contains("self.servers.push(servers::COPILOT.to_string())"), "{}", out[1]);
}

#[test]
fn ignored_results_in_arms_keep_braces() {
    // `push` is also a throwing function of the file, so the ignored `Vec::push` becomes `val _ =`
    check(
        r#"
fn push(line: &str) -> Result<(), String> {
    if line.is_empty() { Err("empty".into()) } else { Ok(()) }
}

fn record(n: usize, seen: &mut Vec<usize>) {
    match n {
        3 => {
            seen.push(n);
        }
        _ => {}
    }
}
"#,
        r#"fun push(line: &str) throws String {
    if (line.is_empty()) throw "empty"
}

fun record(n: usize, seen: &mut Vec<usize>) {
    when (n) {
        3 -> {
            val _ = seen.push(n)
        }
        else -> {}
    }
}
"#,
    );
}

#[test]
fn item_macros_stay_items() {
    // Oxie calls an item macro as Rust does, so it needs no `rust { }` block
    check(
        r#"
thread_local! {
    static COUNT: std::cell::Cell<u32> = std::cell::Cell::new(0);
}

fn bump() -> u32 {
    COUNT.with(|c| {
        c.set(c.get() + 1);
        c.get()
    })
}
"#,
        r#"thread_local! {
    static COUNT : std::cell::Cell < u32 > = std::cell::Cell::new(0);
}

fun bump(): u32 {
    COUNT.with { c ->
        c.set(c.get() + 1)
        c.get()
    }
}
"#,
    );
}
