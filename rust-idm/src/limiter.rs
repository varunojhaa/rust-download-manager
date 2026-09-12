use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// Simple shared token-bucket used to cap total download speed across all
/// connections. `bytes_per_sec == 0` means unlimited.
pub struct RateLimiter {
    bytes_per_sec: u64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec,
            bucket: Mutex::new(Bucket { tokens: bytes_per_sec as f64, last: Instant::now() }),
        }
    }

    pub fn unlimited() -> Self {
        Self::new(0)
    }

    pub async fn acquire(&self, amount: u64) {
        if self.bytes_per_sec == 0 || amount == 0 {
            return;
        }
        let rate = self.bytes_per_sec as f64;
        let mut want = amount as f64;

        while want > 0.0 {
            let wait = {
                let mut bucket = self.bucket.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(bucket.last).as_secs_f64();
                bucket.last = now;
                bucket.tokens = (bucket.tokens + elapsed * rate).min(rate);

                if bucket.tokens > 0.0 {
                    let take = bucket.tokens.min(want);
                    bucket.tokens -= take;
                    want -= take;
                    Duration::ZERO
                } else {
                    Duration::from_millis(20)
                }
            };

            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
    }
}
