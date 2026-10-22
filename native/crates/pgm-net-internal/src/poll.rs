//! Long polls that never miss a wake-up: listen first, then look, then wait.
use std::time::Duration;
use tokio::sync::Notify;

/// What `look` finds now, else the first thing it finds after `notify` wakes (notify_waiters / notify_one), or None
/// once `timeout` has passed. The listener is in place before the first look, so a change between the look and the
/// wait is never lost.
pub async fn wait_for<T>(notify: &Notify, timeout: Duration, look: impl Fn() -> Option<T>) -> Option<T> {
    let end = tokio::time::Instant::now() + timeout;
    loop {
        let n = notify.notified(); tokio::pin!(n); n.as_mut().enable();
        if let Some(v) = look() { return Some(v); }
        if tokio::time::timeout_at(end, n).await.is_err() { return look(); }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    #[tokio::test]
    async fn wakes_and_times_out() {
        let n = Arc::new(tokio::sync::Notify::new()); let v = Arc::new(AtomicU32::new(0));
        let (n2, v2) = (n.clone(), v.clone());
        tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_millis(50)).await; v2.store(7, Ordering::SeqCst); n2.notify_waiters(); });
        let got = super::wait_for(&n, std::time::Duration::from_secs(5), || { let x = v.load(Ordering::SeqCst); (x != 0).then_some(x) }).await;
        assert_eq!(got, Some(7));
        let none: Option<u32> = super::wait_for(&n, std::time::Duration::from_millis(30), || None).await;
        assert!(none.is_none());
    }
}
