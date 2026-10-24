// Values passed down most calls, like GPUI's `window` and `cx`, which the translation makes
// context parameters: methods drop them, and so do the calls that pass them along.
use std::ops::{Deref, DerefMut};

struct Window {
    title: String,
    repaints: u32,
}

impl Window {
    fn repaint(&mut self) {
        self.repaints += 1;
    }
}

struct App {
    log: Vec<String>,
}

impl App {
    fn note(&mut self, s: &str) {
        self.log.push(s.to_string());
    }
}

/// A view's handle on the app, which reads as the app itself.
struct Context<T> {
    app: App,
    notified: u32,
    view: std::marker::PhantomData<T>,
}

impl<T> Context<T> {
    fn notify(&mut self) {
        self.notified += 1;
    }

    /// Runs `f` now with a context of its own, as GPUI runs an update later.
    fn update(&mut self, f: impl FnOnce(&mut Self)) {
        f(self);
    }
}

impl<T> Deref for Context<T> {
    type Target = App;
    fn deref(&self) -> &App {
        &self.app
    }
}

impl<T> DerefMut for Context<T> {
    fn deref_mut(&mut self) -> &mut App {
        &mut self.app
    }
}

type Handler = fn(&mut Workspace, &str, &mut Window, &mut Context<Workspace>);

pub struct Workspace {
    tabs: Vec<String>,
    active: usize,
    handlers: Vec<Handler>,
}

impl Workspace {
    pub fn open(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.push(path.to_string());
        self.activate(self.tabs.len() - 1, window, cx);
        cx.note(&format!("opened {}", path));
    }

    pub fn close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while !self.tabs.is_empty() {
            self.close_active(window, cx);
        }
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        self.update_title(window, cx);
        self.changed(cx);
    }

    fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed = self.tabs.remove(self.active);
        cx.note(&format!("closed {}", closed));
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.update_title(window, cx);
    }

    fn update_title(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.title = match self.tabs.get(self.active) {
            Some(t) => format!("{} ({} open)", t, self.tabs.len()),
            None => "empty".to_string(),
        };
        window.repaint();
        self.changed(cx);
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Takes the window it doesn't use, as handlers must.
    fn on_save(&mut self, name: &str, _window: &mut Window, cx: &mut Context<Self>) {
        cx.note(&format!("saved {} of {}", name, self.tab_count()));
    }

    fn on_reload(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.note(&format!("reload {}", name));
        self.update_title(window, cx);
    }

    fn listen(&mut self, handler: Handler) {
        self.handlers.push(handler);
    }

    fn fire(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        for h in self.handlers.clone() {
            h(self, name, window, cx);
        }
    }

    /// A closure's own `cx` hides the method's.
    fn later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.update(|cx| cx.note("later"));
        self.update_title(window, cx);
    }

    fn forget(&mut self, _cx: &mut Context<Self>) {
        self.handlers.clear();
    }
}

#[allow(dead_code)]
impl Workspace {
    fn reopen(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.iter().any(|t| t == path) {
            self.forget(cx);
        }
        self.open(path, window, cx);
        self.fire(path, window, cx);
    }
}

fn main() {
    let mut window = Window { title: String::new(), repaints: 0 };
    let mut cx: Context<Workspace> = Context { app: App { log: Vec::new() }, notified: 0, view: std::marker::PhantomData };
    let mut ws = Workspace { tabs: Vec::new(), active: 0, handlers: Vec::new() };
    ws.listen(Workspace::on_save);
    ws.listen(Workspace::on_reload);
    ws.open("a.rs", &mut window, &mut cx);
    ws.open("b.rs", &mut window, &mut cx);
    println!("{}", window.title);
    ws.fire("b.rs", &mut window, &mut cx);
    ws.later(&mut window, &mut cx);
    ws.reopen("a.rs", &mut window, &mut cx);
    ws.close_all(&mut window, &mut cx);
    println!("{} / {} repaints / {} notified", window.title, window.repaints, cx.notified);
    for l in &cx.log {
        println!("{}", l);
    }
}
