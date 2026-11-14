//! The teleprompter script, in teleprompter.txt like the .NET build.

const FILE: &str = "teleprompter.txt";

pub fn load() -> String {
    std::fs::read_to_string(super::store::path(FILE)).unwrap_or_default()
}

pub fn save(script: &str) {
    let _ = std::fs::write(super::store::path(FILE), script);
}

/// Scroll speed in DIPs per second for the 0..1 speed setting.
pub fn pixels_per_second(speed: f64) -> f32 {
    (8.0 + speed.clamp(0.0, 1.0) * 90.0) as f32
}
