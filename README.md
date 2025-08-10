# activity_logger

activity_logger is a personal usage analytics tracker that monitors keyboard and mouse activity on your computer.

## what it tracks

- mouse clicks (left, right) and drag events
- keystrokes with typing rhythm analysis
- mouse movement distance
- backspace usage and function key presses
- modifier key usage (ctrl, alt, shift, etc.)
- typing speed (wpm) and typing burst patterns
- undo operations

all data is stored locally in an sqlite database (`activity.db`) with configurable tracking intervals (default: 10 minutes).

## prerequisites

- rust and cargo
- sqlite

## setup instructions

### macos

1. build the project: `cargo build --release`
2. run: `./target/release/activity_logger`
3. when prompted, grant accessibility permissions in the system dialog that appears

### linux

1. install system dependencies: `sudo apt-get install libxdo-dev` (ubuntu/debian)
2. build the project: `cargo build --release`
3. run: `./target/release/activity_logger`

> [!WARNING]
> x11 required. does not work with wayland or console environments

### windows

1. build the project: `cargo build --release`
2. run: `.\target\release\activity_logger.exe`

## configuration

- **tracking interval**: modify `TRACKING_INTERVAL_MINUTES` in `src/main.rs` (default: 10 minutes)
- **save frequency**: modify `SAVE_INTERVAL_SECONDS` in `src/main.rs` (default: 60 seconds)

> [!CAUTION]
> changing intervals requires rebuilding the project with `cargo build --release` and restarting the background service(if any).

> [!TIP]
> it's recommended to run this in the background
