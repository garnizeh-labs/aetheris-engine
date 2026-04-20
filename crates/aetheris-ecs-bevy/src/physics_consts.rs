//! Definitional physics constants for the Aetheris Engine (VS-01).
//!
//! These values are aligned with design documents M1015, M1020, and M1038
//! to ensure balanced Newtonian mechanics and cargo penalties.

/// Default mass for a ship (e.g., Interceptor) in empty state.
pub const DEFAULT_BASE_MASS: f32 = 100.0;

/// Default thrust force (Newtons) for standard ship engines.
/// Resulting acceleration (empty): 500.0 / 100.0 = 5.0 m/s^2.
pub const DEFAULT_THRUST_FORCE: f32 = 500.0;

/// Maximum velocity allowed in the 2D grid simulation for comfort.
pub const DEFAULT_MAX_VELOCITY: f32 = 30.0;

/// Linear drag coefficient (M1015 OQ-2).
/// Applied as: velocity *= (1.0 - drag * dt).
pub const DEFAULT_DRAG: f32 = 0.05;

/// Mass added to the entity for each unit of ore in the `CargoHold` (c).
pub const MASS_PER_ORE: f32 = 1.0;
