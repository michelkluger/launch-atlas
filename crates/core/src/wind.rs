/// Wind specification for a query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wind {
    /// Meteorological direction: where the wind is coming FROM, degrees from N.
    pub from_deg: f32,
    pub speed_kmh: f32,
}

impl Wind {
    pub fn new(from_deg: f32, speed_kmh: f32) -> Self {
        Self {
            from_deg,
            speed_kmh,
        }
    }
}
