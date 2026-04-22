use aetheris_protocol::types::{
    AIState, ClientId, InputCommand, NetworkId, OreType, ProjectileType, RespawnLocation,
    RoomBounds, RoomDefinition, RoomMembership, SectorId, ShipClass, ShipStats, Transform,
    WeaponId,
};
use bevy_ecs::prelude::Component;
use serde::{Deserialize, Serialize};

macro_rules! impl_component_serde {
    ($t:ty) => {
        impl TryFrom<Vec<u8>> for $t {
            type Error = rmp_serde::decode::Error;
            fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
                rmp_serde::from_slice(&value)
            }
        }
        impl TryInto<Vec<u8>> for $t {
            type Error = rmp_serde::encode::Error;
            fn try_into(self) -> Result<Vec<u8>, Self::Error> {
                rmp_serde::to_vec(&self)
            }
        }
    };
}

// ──────────────────────────────────────────────
// Replicated Components (Data Plane — M1020 §3.3)
// ──────────────────────────────────────────────

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransformComponent(pub Transform);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, Default)]
pub struct Velocity {
    pub dx: f32,
    pub dy: f32,
    pub dz: f32,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct ShipStatsComponent(pub ShipStats);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Loadout {
    pub ship_class: ShipClass,
    pub weapon_ids: [WeaponId; 6],
    pub weapon_count: u8,
    pub hull_tier: u8,
    pub engine_tier: u8,
    pub shield_tier: u8,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ShipClassComponent(pub ShipClass);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct PlayerName {
    pub name: String,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FactionTag {
    pub faction_id: u8,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AsteroidHP {
    pub hp: u16,
    pub max_hp: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AsteroidYield {
    pub ore_type: OreType,
    pub ore_per_destroy: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LootDrop {
    pub ore_type: OreType,
    pub quantity: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MapSeedComponent(pub u64);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Station {
    pub position: [f32; 2],
    pub safe_zone_radius: f32,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct JumpGate {
    pub destination_sector: SectorId,
    pub activation_radius: f32,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ProjectileMarker {
    pub projectile_type: ProjectileType,
    pub origin_tick: u64,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DockedState {
    pub station_id: NetworkId,
    pub docked_at_tick: u64,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, Default)]
pub struct MiningBeam {
    pub active: bool,
    pub target: Option<NetworkId>,
    pub last_seen_input_tick: Option<u64>,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CargoHold {
    pub ore_count: u16,
    pub capacity: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Asteroid {
    pub ore_remaining: u16,
    pub total_capacity: u16,
}

impl_component_serde!(TransformComponent);
impl_component_serde!(Velocity);
impl_component_serde!(ShipStatsComponent);
impl_component_serde!(Loadout);
impl_component_serde!(ShipClassComponent);
impl_component_serde!(PlayerName);
impl_component_serde!(FactionTag);
impl_component_serde!(AsteroidHP);
impl_component_serde!(AsteroidYield);
impl_component_serde!(LootDrop);
impl_component_serde!(Station);
impl_component_serde!(JumpGate);
impl_component_serde!(ProjectileMarker);
impl_component_serde!(DockedState);
impl_component_serde!(MiningBeam);
impl_component_serde!(CargoHold);
impl_component_serde!(Asteroid);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomDefinitionComponent(pub RoomDefinition);
impl_component_serde!(RoomDefinitionComponent);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomBoundsComponent(pub RoomBounds);
impl_component_serde!(RoomBoundsComponent);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomMembershipComponent(pub RoomMembership);
impl_component_serde!(RoomMembershipComponent);

// ──────────────────────────────────────────────
// Server-Only Components (M1020 §3.4)
// ──────────────────────────────────────────────

#[derive(Component, Debug, Clone)]
pub struct Wallet {
    pub credits: u64,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct PhysicsBody {
    pub base_mass: f32,
    pub thrust_force: f32,
    pub max_velocity: f32,
    pub turn_rate: f32,
    pub drag: f32,
    pub mass_per_ore: f32,
}

#[derive(Component, Debug, Clone)]
pub struct WeaponSlot {
    pub weapon_type: WeaponId, // Using WeaponId for consistency
    pub cooldown_ticks: u16,
    pub current_cooldown: u16,
}

#[derive(Component, Debug, Clone)]
pub struct WeaponCooldown {
    pub ticks_remaining: Vec<u16>,
}

#[derive(Component, Debug, Clone)]
pub struct AmmoCount {
    pub missiles_remaining: u16,
    pub reserve: u16,
}

#[derive(Component, Debug, Clone)]
pub struct AIBehavior {
    pub state: AIState,
    pub aggro_radius: f32,
    pub leash_radius: f32,
    pub weapon_range: f32,
}

#[derive(Component, Debug, Clone)]
pub struct AIHomeWaypoints {
    pub waypoints: Vec<[f32; 2]>,
    pub current: u8,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct SafeZoneFlag {
    pub is_inside: bool,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct SuspicionScore {
    pub score: u32,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct MerkleChainState {
    pub prev_hash: [u8; 32],
    pub last_tick: u64,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct AsteroidRespawn {
    pub delay_ticks: u32,
    pub remaining: u32,
    pub x: f32,
    pub y: f32,
    pub total_capacity: u16,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct ShieldRegenTimer {
    pub ticks_until_regen: u16,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct RespawnTimer {
    pub delay_ticks: u16,
    pub location: RespawnLocation,
}

#[derive(Component, Debug, Clone)]
pub struct InputHistory {
    pub ring: std::collections::VecDeque<InputCommand>,
}

#[derive(Component, Debug, Clone)]
pub struct LatestInput {
    pub command: InputCommand,
    pub last_client_tick: u64,
}

/// Resource used to track the authoritative server tick.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, Copy, Default)]
pub struct ServerTick(pub u64);

/// Resource used to queue reliable messages from ECS systems to be sent over the wire.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, Default)]
pub struct ReliableEvents {
    pub queue: Vec<(
        Option<aetheris_protocol::types::ClientId>,
        aetheris_protocol::events::GameEvent,
    )>,
}

/// Resource used to optimize Stage 4 entity extraction by grouping entities by Room.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, Default)]
pub struct RoomIndex {
    pub memberships:
        std::collections::HashMap<NetworkId, std::collections::HashSet<bevy_ecs::prelude::Entity>>,
}

#[derive(Component, Debug, Clone)]
pub struct DamageTracker {
    pub last_damager: Option<NetworkId>, // Using NetworkId for consistency
    pub last_damage_tick: u64,
    pub accumulated: std::collections::HashMap<NetworkId, u32>,
}

/// Mapped from the definitive decision: Move `NetworkOwner` to Server-Only.
#[derive(Component, Debug, Clone, Copy)]
pub struct NetworkOwner(pub ClientId);

/// Marker: this entity is the authoritative session ship for a client.
///
/// Only set on the entity spawned via `StartSession` / `spawn_session_ship`.
/// Playground entities spawned via the `Spawn` event never receive this
/// marker, which allows the input pipeline to gate `InputCommand` processing
/// exclusively to the session ship.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SessionShip;
