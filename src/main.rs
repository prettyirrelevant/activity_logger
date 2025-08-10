//! Simple activity tracker for personal usage analytics

use jiff::Timestamp;
use rdev::{Event, EventType, Key, listen};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::time::interval;
use tracing::{error, info};

static EVENT_SENDER: OnceLock<tokio::sync::mpsc::UnboundedSender<Event>> = OnceLock::new();

fn event_callback(event: Event) {
    if let Some(sender) = EVENT_SENDER.get() {
        let _ = sender.send(event);
    }
}

const TRACKING_INTERVAL_MINUTES: i64 = 10;
const SAVE_INTERVAL_SECONDS: u64 = 60;

#[derive(Debug, Serialize, Clone)]
struct Stats {
    id: String,
    timestamp: i64,
    clicks: u64,
    keypresses: u64,
    mouse_distance: f64,
    backspaces: u64,
    function_keys: u64,
    undo_operations: u64,
    modifier_keys: u64,
    drag_events: u64,
    right_clicks: u64,
    typing_bursts: u64,
    keypress_intervals_ms: Vec<u64>,
}

impl Stats {
    fn new() -> Self {
        Self {
            id: generate_id(),
            timestamp: current_period_timestamp(),
            clicks: 0,
            keypresses: 0,
            mouse_distance: 0.0,
            backspaces: 0,
            function_keys: 0,
            undo_operations: 0,
            modifier_keys: 0,
            drag_events: 0,
            right_clicks: 0,
            typing_bursts: 0,
            keypress_intervals_ms: Vec::new(),
        }
    }

    fn average_typing_speed_wpm(&self) -> f64 {
        if self.keypress_intervals_ms.len() < 2 {
            return 0.0;
        }

        let total_time_ms: u64 = self.keypress_intervals_ms.iter().sum();
        if total_time_ms == 0 {
            return 0.0;
        }

        // Assume average word length of 5 characters
        let words = self.keypresses as f64 / 5.0;
        let minutes = total_time_ms as f64 / 60000.0;

        if minutes > 0.0 { words / minutes } else { 0.0 }
    }

    fn typing_rhythm_variance(&self) -> f64 {
        if self.keypress_intervals_ms.len() < 2 {
            return 0.0;
        }

        let mean = self.keypress_intervals_ms.iter().sum::<u64>() as f64
            / self.keypress_intervals_ms.len() as f64;

        let variance = self
            .keypress_intervals_ms
            .iter()
            .map(|&x| (x as f64 - mean).powi(2))
            .sum::<f64>()
            / self.keypress_intervals_ms.len() as f64;

        variance.sqrt()
    }
}

fn generate_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn current_period_timestamp() -> i64 {
    let now = Timestamp::now();
    let interval_seconds = TRACKING_INTERVAL_MINUTES * 60;
    (now.as_second() / interval_seconds) * interval_seconds
}

fn format_datetime(timestamp: i64) -> String {
    let dt = Timestamp::from_second(timestamp)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system());
    if TRACKING_INTERVAL_MINUTES >= 60 {
        dt.strftime("%Y-%m-%d %H:00 %Z").to_string()
    } else {
        dt.strftime("%Y-%m-%d %H:%M %Z").to_string()
    }
}

struct Tracker {
    stats: Arc<Mutex<Stats>>,
    db: Arc<Mutex<Connection>>,
    last_mouse: Arc<Mutex<(f64, f64)>>,
    last_keypress_time: Arc<Mutex<Option<std::time::Instant>>>,
    drag_in_progress: Arc<Mutex<bool>>,
    ctrl_pressed: Arc<Mutex<bool>>,
}

impl Tracker {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let db = Connection::open("activity.db")?;

        db.execute(
            "CREATE TABLE IF NOT EXISTS activity_stats (
                id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL UNIQUE,
                clicks INTEGER,
                keypresses INTEGER,
                mouse_distance REAL,
                backspaces INTEGER,
                function_keys INTEGER,
                undo_operations INTEGER,
                modifier_keys INTEGER,
                drag_events INTEGER,
                right_clicks INTEGER,
                typing_bursts INTEGER,
                avg_typing_speed_wpm REAL,
                typing_rhythm_variance REAL
            )",
            [],
        )?;

        info!("activity logger v{} started", env!("CARGO_PKG_VERSION"));
        info!(
            "configuration: tracking_interval={}min save_interval={}s",
            TRACKING_INTERVAL_MINUTES, SAVE_INTERVAL_SECONDS
        );
        info!("database initialized: path=activity.db");

        Ok(Self {
            stats: Arc::new(Mutex::new(Stats::new())),
            db: Arc::new(Mutex::new(db)),
            last_mouse: Arc::new(Mutex::new((0.0, 0.0))),
            last_keypress_time: Arc::new(Mutex::new(None)),
            drag_in_progress: Arc::new(Mutex::new(false)),
            ctrl_pressed: Arc::new(Mutex::new(false)),
        })
    }

    fn handle_event(&self, event: Event) {
        match event.event_type {
            EventType::MouseMove { x, y } => {
                let mut last = self.last_mouse.lock().unwrap();
                if last.0 != 0.0 || last.1 != 0.0 {
                    let distance = ((x - last.0).powi(2) + (y - last.1).powi(2)).sqrt();
                    if distance < 1000.0 {
                        // ignore teleports
                        self.stats.lock().unwrap().mouse_distance += distance;
                    }
                }
                *last = (x, y);
            }

            EventType::ButtonPress(button) => {
                let mut stats = self.stats.lock().unwrap();
                stats.clicks += 1;

                match button {
                    rdev::Button::Right => stats.right_clicks += 1,
                    rdev::Button::Left => {
                        *self.drag_in_progress.lock().unwrap() = true;
                    }
                    _ => {}
                }
            }

            EventType::ButtonRelease(button) => {
                if button == rdev::Button::Left {
                    let mut drag_in_progress = self.drag_in_progress.lock().unwrap();
                    if *drag_in_progress {
                        self.stats.lock().unwrap().drag_events += 1;
                        *drag_in_progress = false;
                    }
                }
            }

            EventType::KeyPress(key) => {
                let now = std::time::Instant::now();
                let mut stats = self.stats.lock().unwrap();

                // Track typing rhythm
                if let Some(last_time) = *self.last_keypress_time.lock().unwrap() {
                    let interval = now.duration_since(last_time).as_millis() as u64;
                    if interval < 5000 {
                        // Only track if within 5 seconds
                        stats.keypress_intervals_ms.push(interval);
                        if interval < 200 {
                            // Fast typing burst
                            stats.typing_bursts += 1;
                        }
                    }
                }
                *self.last_keypress_time.lock().unwrap() = Some(now);

                stats.keypresses += 1;

                match key {
                    Key::Backspace => stats.backspaces += 1,
                    Key::F1
                    | Key::F2
                    | Key::F3
                    | Key::F4
                    | Key::F5
                    | Key::F6
                    | Key::F7
                    | Key::F8
                    | Key::F9
                    | Key::F10
                    | Key::F11
                    | Key::F12 => {
                        stats.function_keys += 1;
                    }
                    Key::ControlLeft | Key::ControlRight => {
                        *self.ctrl_pressed.lock().unwrap() = true;
                        stats.modifier_keys += 1;
                    }
                    Key::Alt
                    | Key::AltGr
                    | Key::MetaLeft
                    | Key::MetaRight
                    | Key::ShiftLeft
                    | Key::ShiftRight => {
                        stats.modifier_keys += 1;
                    }
                    Key::KeyZ => {
                        if *self.ctrl_pressed.lock().unwrap() {
                            stats.undo_operations += 1;
                        }
                    }
                    _ => {}
                }
            }

            EventType::KeyRelease(Key::ControlLeft | Key::ControlRight) => {
                *self.ctrl_pressed.lock().unwrap() = false;
            }

            _ => {}
        }
    }

    async fn save(&self) {
        let stats = self.stats.lock().unwrap().clone();
        let typing_speed = stats.average_typing_speed_wpm();
        let typing_variance = stats.typing_rhythm_variance();

        if let Ok(db) = self.db.lock() {
            match db.execute(
                "INSERT OR REPLACE INTO activity_stats
                 (id, timestamp, clicks, keypresses, mouse_distance, backspaces, function_keys,
                  undo_operations, modifier_keys, drag_events, right_clicks, typing_bursts,
                  avg_typing_speed_wpm, typing_rhythm_variance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    stats.id,
                    stats.timestamp,
                    stats.clicks,
                    stats.keypresses,
                    stats.mouse_distance,
                    stats.backspaces,
                    stats.function_keys,
                    stats.undo_operations,
                    stats.modifier_keys,
                    stats.drag_events,
                    stats.right_clicks,
                    stats.typing_bursts,
                    typing_speed,
                    typing_variance
                ],
            ) {
                Ok(_) => info!(
                    "stats saved: period={} clicks={} right_clicks={} drags={} keypresses={} typing_speed_wpm={:.1} typing_rhythm_variance={:.1}ms undos={} modifier_keys={} mouse_distance={:.0}px",
                    format_datetime(stats.timestamp),
                    stats.clicks,
                    stats.right_clicks,
                    stats.drag_events,
                    stats.keypresses,
                    typing_speed,
                    typing_variance,
                    stats.undo_operations,
                    stats.modifier_keys,
                    stats.mouse_distance
                ),
                Err(e) => error!(
                    "database save failed: error={} table=activity_stats period={}",
                    e,
                    format_datetime(stats.timestamp)
                ),
            }
        }
    }

    fn reset_if_new_period(&self) {
        let current_period = current_period_timestamp();
        let mut stats = self.stats.lock().unwrap();

        if stats.timestamp != current_period {
            *stats = Stats::new();
            info!(
                "period reset: new_period={} previous_period={}",
                format_datetime(current_period),
                format_datetime(stats.timestamp)
            );
        }
    }

    async fn run_background_tasks(&self) {
        let mut save_timer = interval(Duration::from_secs(SAVE_INTERVAL_SECONDS));
        let mut period_timer = interval(Duration::from_secs(300));

        loop {
            tokio::select! {
                _ = save_timer.tick() => self.save().await,
                _ = period_timer.tick() => self.reset_if_new_period(),
            }
        }
    }

    async fn run(self: Arc<Self>) {
        // Shutdown handler
        let shutdown_tracker = Arc::clone(&self);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("shutdown signal received: signal=SIGINT action=saving_final_data");
                shutdown_tracker.save().await;
                info!("shutdown complete: status=graceful");
                std::process::exit(0);
            }
        });

        // Background tasks
        let bg_tracker = Arc::clone(&self);
        tokio::spawn(async move {
            bg_tracker.run_background_tasks().await;
        });

        // Event listener
        let event_tracker = Arc::clone(&self);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        EVENT_SENDER.set(tx).expect("Failed to set event sender");

        info!("event monitoring started: method=rdev_listen stop_command=SIGINT");

        std::thread::spawn(move || {
            if let Err(e) = listen(event_callback) {
                error!(
                    "event listener failed: error={:?} component=rdev_listener",
                    e
                );
            }
        });

        while let Some(event) = rx.recv().await {
            event_tracker.handle_event(event);
        }
    }
}

struct CustomTimer;

impl tracing_subscriber::fmt::time::FormatTime for CustomTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
        write!(w, "{}", now.strftime("%Y-%m-%d %H:%M:%S %Z"))
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_timer(CustomTimer).init();

    match Tracker::new() {
        Ok(tracker) => Arc::new(tracker).run().await,
        Err(e) => error!("failed to start: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_period_timestamp() {
        let ts1 = current_period_timestamp();
        let ts2 = current_period_timestamp();
        assert_eq!(ts1, ts2);

        let interval_seconds = TRACKING_INTERVAL_MINUTES * 60;
        assert_eq!(ts1 % interval_seconds, 0);
    }

    #[test]
    fn test_stats_new() {
        let stats = Stats::new();
        assert_eq!(stats.clicks, 0);
        assert_eq!(stats.keypresses, 0);
        assert_eq!(stats.mouse_distance, 0.0);
        assert_eq!(stats.undo_operations, 0);
        assert_eq!(stats.modifier_keys, 0);
        assert_eq!(stats.drag_events, 0);
        assert_eq!(stats.right_clicks, 0);
        assert_eq!(stats.typing_bursts, 0);
        assert!(stats.keypress_intervals_ms.is_empty());
        assert!(!stats.id.is_empty());
    }

    #[test]
    fn test_typing_metrics() {
        let mut stats = Stats::new();

        // Simulate some typing intervals
        stats.keypress_intervals_ms = vec![100, 150, 120, 180, 90];
        stats.keypresses = 25; // 5 words

        let wpm = stats.average_typing_speed_wpm();
        assert!(wpm > 0.0);

        let variance = stats.typing_rhythm_variance();
        assert!(variance > 0.0);
    }

    #[test]
    fn test_generate_id() {
        let id1 = generate_id();
        let id2 = generate_id();
        assert_ne!(id1, id2);
        assert_eq!(id1.len(), 16);
        assert_eq!(id2.len(), 16);
    }
}
