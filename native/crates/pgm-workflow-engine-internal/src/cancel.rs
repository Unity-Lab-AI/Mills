//! Cancel handles. A run has one, every scope (a loop round, a parallel line, a sub-graph call) one made from it, and
//! every running node one made from its scope's: cancelling a handle cancels everything made from it, never its parent.
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct Cancel(Arc<Inner>);

#[derive(Default)]
struct Inner {
    flag: AtomicBool,
    notify: Notify,
    children: Mutex<Vec<Weak<Inner>>>,
}

impl Cancel {
    pub fn new() -> Cancel { Cancel::default() }

    /// A handle that is cancelled with this one (or by itself).
    pub fn child(&self) -> Cancel {
        let c = Cancel::new();
        let mut kids = self.0.children.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_cancelled() {
            drop(kids);
            c.cancel();
        } else {
            kids.retain(|w| w.strong_count() > 0);
            kids.push(Arc::downgrade(&c.0));
        }
        c
    }

    pub fn cancel(&self) {
        if self.0.flag.swap(true, Ordering::SeqCst) { return; }
        self.0.notify.notify_waiters();
        let kids = std::mem::take(&mut *self.0.children.lock().unwrap_or_else(|e| e.into_inner()));
        for k in kids { if let Some(k) = k.upgrade() { Cancel(k).cancel(); } }
    }

    pub fn is_cancelled(&self) -> bool { self.0.flag.load(Ordering::SeqCst) }

    /// Resolves once the handle is cancelled (at once when it already is).
    pub async fn cancelled(&self) {
        loop {
            let n = self.0.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.is_cancelled() { return; }
            n.await;
        }
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "Cancel({})", if self.is_cancelled() { "cancelled" } else { "live" }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn children_follow_parents_not_the_other_way() {
        let run = Cancel::new();
        let scope = run.child();
        let node = scope.child();
        let other = run.child();
        scope.cancel();
        assert!(scope.is_cancelled() && node.is_cancelled());
        assert!(!run.is_cancelled() && !other.is_cancelled());
        node.cancelled().await;
        run.cancel();
        other.cancelled().await;
        assert!(run.child().is_cancelled(), "a child of a cancelled handle starts cancelled");
    }
}
