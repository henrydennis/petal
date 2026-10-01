//! Progress and time-left estimates for a scan.
//!
//! Progress counts items (files and folders) against the volume's object count, which
//! APFS reports exactly and instantly. Items track progress far better than bytes:
//! unreadable folders hide far more bytes than items, so a bytes bar would stall at ~80%.

/// Smooths the item rate so "time left" doesn't jump around as the scan moves between
/// fast (cached) and slow areas.
#[derive(Default, Clone)]
pub struct Eta {
    last: Option<(f64, u64)>,
    rate: Option<f64>,
}

/// Seconds over which the rate is averaged.
const SMOOTHING: f64 = 3.0;
/// Don't guess before this much of the scan has run.
const MIN_ELAPSED: f64 = 1.5;

impl Eta {
    /// Feed the items scanned so far at `t` seconds into the scan.
    pub fn update(&mut self, t: f64, items: u64) {
        if let Some((last_t, last_items)) = self.last {
            let dt = t - last_t;
            if dt <= 0.0 {
                return;
            }
            let instant = items.saturating_sub(last_items) as f64 / dt;
            let alpha = 1.0 - (-dt / SMOOTHING).exp();
            self.rate = Some(match self.rate {
                Some(rate) => rate + alpha * (instant - rate),
                None => instant,
            });
        }
        self.last = Some((t, items));
    }

    /// Estimated seconds left, once there's enough to go on.
    pub fn remaining(&self, t: f64, items: u64, total: u64) -> Option<f64> {
        let rate = self.rate.filter(|r| *r > 0.0)?;
        if t < MIN_ELAPSED || items * 100 < total {
            return None;
        }
        Some(total.saturating_sub(items) as f64 / rate)
    }
}

/// Share of the volume's items scanned, held below 100% until the scan really finishes.
pub fn fraction(items: u64, total: u64) -> f32 {
    (items as f64 / total.max(1) as f64).min(0.99) as f32
}

pub fn describe(remaining: f64) -> String {
    if remaining < 2.0 {
        "finishing up…".to_string()
    } else if remaining < 60.0 {
        format!("about {} s left", (remaining / 5.0).ceil() as u64 * 5)
    } else {
        format!("about {} min left", (remaining / 60.0).ceil() as u64)
    }
}
