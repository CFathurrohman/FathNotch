//! Damped spring: every size and shape change in the notch moves through one of these.

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    /// `damping` close to 2·√stiffness settles without bouncing; a little under gives a soft overshoot.
    pub const fn new(value: f32, stiffness: f32, damping: f32) -> Self {
        Self { value, target: value, velocity: 0.0, stiffness, damping }
    }

    pub fn moving(&self) -> bool {
        self.value != self.target || self.velocity != 0.0
    }

    /// Advances by `dt` seconds in small sub-steps (stable on slow frames). Returns true while moving.
    pub fn step(&mut self, dt: f32) -> bool {
        if !self.moving() {
            return false;
        }
        let steps = (dt / (1.0 / 240.0)).ceil().max(1.0) as u32;
        let h = dt / steps as f32;
        for _ in 0..steps {
            let force = -self.stiffness * (self.value - self.target) - self.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
        }
        // Snap only when both the gap and the speed are tiny, so the last frame never visibly jumps.
        if (self.value - self.target).abs() < 0.02 && self.velocity.abs() < 0.2 {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}
