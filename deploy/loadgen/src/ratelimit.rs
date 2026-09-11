//! 令牌桶: 速率可热更新; 起始为空, 避免启动瞬间突发.

use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug)]
pub struct TokenBucket {
    rate: f64,
    capacity: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: f64) -> Self {
        let rate = rate.max(0.0);
        Self {
            rate,
            capacity: rate.max(1.0),
            tokens: 0.0,
            last: Instant::now(),
        }
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// 更新速率; 桶容量同步为 1 秒额度.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate.max(0.0);
        self.capacity = self.rate.max(1.0);
        self.tokens = self.tokens.min(self.capacity);
    }

    /// 取走 n 个令牌; 不足时返回建议等待时长 (调用方 sleep 后重试).
    pub fn try_take(&mut self, n: f64, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= n {
            self.tokens -= n;
            return Ok(());
        }
        if self.rate <= 0.0 {
            return Err(Duration::from_millis(50));
        }
        let missing = n - self.tokens;
        Err(Duration::from_secs_f64(missing / self.rate))
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
            self.last = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn rate_is_accurate_over_one_second() {
        let mut bucket = TokenBucket::new(1000.0);
        let mut granted = 0u32;
        for _ in 0..1000 {
            if bucket.try_take(1.0, Instant::now()).is_ok() {
                granted += 1;
            }
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        assert!(granted.abs_diff(1000) <= 50, "granted={granted}");
    }

    #[tokio::test(start_paused = true)]
    async fn set_rate_takes_effect_immediately() {
        let mut bucket = TokenBucket::new(100.0);
        bucket.set_rate(1000.0);
        let mut granted = 0u32;
        for _ in 0..100 {
            if bucket.try_take(1.0, Instant::now()).is_ok() {
                granted += 1;
            }
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        assert!(granted >= 95, "granted={granted}");
    }

    #[tokio::test(start_paused = true)]
    async fn tokens_are_capped_by_capacity() {
        let mut bucket = TokenBucket::new(100.0);
        tokio::time::advance(Duration::from_secs(3600)).await;
        let now = Instant::now();
        let mut granted = 0u32;
        while bucket.try_take(1.0, now).is_ok() {
            granted += 1;
        }
        assert_eq!(granted, 100, "容量 = 1 秒额度");
    }

    #[test]
    fn zero_rate_reports_wait() {
        let mut bucket = TokenBucket::new(0.0);
        assert!(bucket.try_take(1.0, Instant::now()).is_err());
        assert_eq!(bucket.rate(), 0.0);
    }
}
