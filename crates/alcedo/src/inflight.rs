//! 同 key 去重（single-flight）：并发撞同一个 key 时只放一个去执行，其余等它
//! 的结果。没有这层，热门内容的一波并发会全部穿透结果缓存，各自把上游打一遍
//! ——每次解析几百毫秒的往返，N 倍请求还放大触发风控的概率。
//!
//! 结构是一张 `key → watch 通道` 的表：
//!
//! - **领队**：抢到表里空位的那个。跑完后把结果（成功或失败）写进通道再摘除
//!   登记项，等待者从通道里拿到同一份结果——包括错误，这样"视频已删除"这类
//!   确定性失败不会被并发请求重放 N 遍。
//! - **等待者**：订阅通道。领队被中途取消（上层超时、请求方断开）时通道的
//!   sender 随之消失，等待者从 [`tokio::sync::watch::Receiver::changed`] 的
//!   `Err` 里察觉，回来重新排队，自然顶上当领队。不存在卡死的登记项：
//!   领队的 [`LeaderGuard`] 无论正常收尾还是被取消都会摘除自己。
//!
//! 等待者不持有 `Arc<Slot>`，只持有 receiver——这是取消信号能传到等待者的
//! 前提（receiver 不会让 sender 活着）。

use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;

use tokio::sync::watch;

/// 一个在跑的 key：结果写进 `tx`，等待者各自持有 receiver。
struct Slot<T> {
    tx: watch::Sender<Option<T>>,
}

/// 同 key 去重表。
pub(crate) struct Flight<T: Clone> {
    map: parking_lot::Mutex<HashMap<String, Arc<Slot<T>>>>,
}

impl<T: Clone> std::fmt::Debug for Flight<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只报数量：key 可能带用户内容，不该进日志
        f.debug_struct("Flight")
            .field("inflight", &self.map.lock().len())
            .finish()
    }
}

impl<T: Clone> Flight<T> {
    pub(crate) fn new() -> Self {
        Self {
            map: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// 要么等来别人的结果，要么抢到执行权。
    ///
    /// 返回 [`Claim::Wait`] 表示结果已经出来（成功或失败），直接用；
    /// 返回 [`Claim::Lead`] 表示你是领队，跑完必须调 [`LeaderGuard::finish`]，
    /// 或者直接丢弃 guard（等于取消，等待者会重新排队）。
    pub(crate) async fn claim(&self, key: &str) -> Claim<'_, T> {
        loop {
            let mut rx = {
                let mut map = self.map.lock();
                match map.entry(key.to_owned()) {
                    Entry::Occupied(e) => e.get().tx.subscribe(),
                    Entry::Vacant(e) => {
                        let (tx, _) = watch::channel(None);
                        e.insert(Arc::new(Slot { tx }));
                        // 守卫在这里建，保证登记项从诞生起就有人负责摘除
                        return Claim::Lead(LeaderGuard {
                            flight: self,
                            key: key.to_owned(),
                        });
                    }
                }
            };
            // 领队可能在我们进锁之前就已经出结果了
            if let Some(v) = rx.borrow_and_update().clone() {
                return Claim::Wait(v);
            }
            // Err = 领队被取消（guard 摘除后 sender 消失）：
            // 不拿值，回环重新排队，下一轮自己上
            if let Ok(()) = rx.changed().await {
                if let Some(v) = rx.borrow_and_update().clone() {
                    return Claim::Wait(v);
                }
                // 领队只写 Some，走到这儿说明登记被别的路径摘了，重排队
            }
        }
    }
}

/// [`Flight::claim`] 的结果。
pub(crate) enum Claim<'a, T: Clone> {
    /// 抢到执行权。跑完调 [`LeaderGuard::finish`] 分享结果。
    Lead(LeaderGuard<'a, T>),
    /// 别人已经跑完，这是它的结果。
    Wait(T),
}

/// 领队的登记项。活着 = 表里有这个 key；无论 drop 还是 [`finish`]，都会摘除。
pub(crate) struct LeaderGuard<'a, T: Clone> {
    flight: &'a Flight<T>,
    key: String,
}

impl<T: Clone> LeaderGuard<'_, T> {
    /// 把结果分享给所有等待者，然后摘除登记。
    pub(crate) fn finish(self, value: T) {
        if let Some(slot) = self.flight.map.lock().get(&self.key) {
            slot.tx.send_replace(Some(value));
        }
        // drop(self) 负责摘除登记
    }
}

impl<T: Clone> Drop for LeaderGuard<'_, T> {
    fn drop(&mut self) {
        self.flight.map.lock().remove(&self.key);
        // 表里那份 Arc 是 Slot 的唯一引用（等待者只持有 receiver），
        // 摘除后 sender 一起消失，等待者从 changed() 的 Err 里察觉并重新排队
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 领队先登记（用 oneshot 确认，避免"谁先 claim"竞态把测试变成抽奖），
    /// 等待者们再排队，最后领队出结果。
    #[tokio::test]
    async fn waiters_receive_the_leaders_result() {
        let flight = Arc::new(Flight::<u32>::new());
        let (ack, ack_rx) = tokio::sync::oneshot::channel::<()>();

        let leader_flight = Arc::clone(&flight);
        let leader = tokio::spawn(async move {
            let Claim::Lead(guard) = leader_flight.claim("k").await else {
                ack.send(()).unwrap();
                return false;
            };
            ack.send(()).unwrap();
            // 给等待者留出排队的时间，让它们真正睡进 changed() 里等唤醒
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            guard.finish(42);
            true
        });
        ack_rx.await.unwrap();

        let mut waiters = Vec::new();
        for _ in 0..4 {
            waiters.push(tokio::spawn({
                let flight = Arc::clone(&flight);
                async move { matches!(flight.claim("k").await, Claim::Wait(42)) }
            }));
        }

        assert!(leader.await.unwrap(), "先 claim 的应当是领队");
        for w in waiters {
            assert!(w.await.unwrap(), "等待者应当拿到领队的结果");
        }
    }

    /// 领队被取消（不调 finish 直接丢掉 guard）时，登记要能被下一个人接走，
    /// 而不是让等待者永远等下去。
    #[tokio::test]
    async fn cancelled_leader_frees_the_slot() {
        let flight: Flight<u32> = Flight::new();
        {
            let Claim::Lead(guard) = flight.claim("k").await else {
                panic!("空表上应当能当上领队");
            };
            drop(guard); // 取消：不 finish
        }
        let took_over = matches!(flight.claim("k").await, Claim::Lead(_));
        assert!(took_over, "应当立刻顶上，说明登记被摘干净了");
    }

    /// 失败的结果也要共享：确定性失败不该被并发请求重放 N 遍。
    #[tokio::test]
    async fn shared_error_reaches_the_waiters() {
        let flight = Arc::new(Flight::<Result<u32, String>>::new());
        let (ack, ack_rx) = tokio::sync::oneshot::channel::<()>();

        let leader_flight = Arc::clone(&flight);
        let leader = tokio::spawn(async move {
            let Claim::Lead(guard) = leader_flight.claim("k").await else {
                ack.send(()).unwrap();
                return;
            };
            ack.send(()).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            guard.finish(Err("已删除".into()));
        });
        ack_rx.await.unwrap();

        match flight.claim("k").await {
            Claim::Wait(Err(e)) => assert_eq!(e, "已删除"),
            _ => panic!("等待者应当拿到共享的错误"),
        }
        leader.await.unwrap();
    }

    /// 不同 key 互不干扰：一个领队挂着，不影响别的 key 立刻执行。
    #[tokio::test]
    async fn different_keys_do_not_block() {
        let flight: Flight<u32> = Flight::new();
        let _leader = flight.claim("a").await;
        assert!(matches!(flight.claim("b").await, Claim::Lead(_)));
    }

    /// 领队 finish 之后，登记被摘除：同一个 key 的下一次请求重新当领队，
    /// 结果不留在表里（交给上层缓存管）。
    #[tokio::test]
    async fn finished_key_can_be_claimed_again() {
        let flight = Arc::new(Flight::<u32>::new());
        let runs = Arc::new(AtomicU32::new(0));
        let (ack, ack_rx) = tokio::sync::oneshot::channel::<()>();

        let leader_flight = Arc::clone(&flight);
        let runs_in_task = Arc::clone(&runs);
        let first = tokio::spawn(async move {
            let Claim::Lead(guard) = leader_flight.claim("k").await else {
                ack.send(()).unwrap();
                return;
            };
            ack.send(()).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            runs_in_task.fetch_add(1, Ordering::Relaxed);
            guard.finish(1);
        });
        ack_rx.await.unwrap();
        // 这次等到的是第一个领队的结果
        assert!(matches!(flight.claim("k").await, Claim::Wait(1)));
        first.await.unwrap();

        // 表已清空，下一次请求重新当领队
        match flight.claim("k").await {
            Claim::Lead(guard) => guard.finish(2),
            Claim::Wait(_) => panic!("结果不该留在表里，应当重新执行"),
        }
        assert_eq!(runs.load(Ordering::Relaxed), 1, "第一个领队只跑了一次");
    }
}
