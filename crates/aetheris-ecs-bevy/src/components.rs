use aetheris_protocol::types::{
    AIState, AgentKind, AgentProperties, ClientId, InputCommand, InteractionBeamType, NetworkId,
    PayloadType, RespawnLocation, ToolId, Transform, WorkspaceBounds, WorkspaceDefinition,
    WorkspaceMembership, ZoneId,
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
pub struct AgentPropertiesComponent(pub AgentProperties);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfiguration {
    pub agent_kind: AgentKind,
    pub tool_ids: [ToolId; 6],
    pub tool_count: u8,
    pub integrity_tier: u8,
    pub mobility_tier: u8,
    pub priority_tier: u8,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AgentKindComponent(pub AgentKind);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct PlayerName {
    pub name: String,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FactionTag {
    pub faction_id: u8,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ResourceIntegrity {
    pub integrity: u16,
    pub max_integrity: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ResourceYield {
    pub payload_type: PayloadType,
    pub payload_per_exhaust: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DataDrop {
    pub payload_type: PayloadType,
    pub amount: u16,
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
pub struct ZoneGate {
    pub destination_zone: ZoneId,
    pub activation_radius: f32,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BeamMarker {
    pub beam_type: InteractionBeamType,
    pub spawn_pos: [f32; 2],
    pub max_range: f32,
    pub owner: NetworkId,
    pub lifetime_ticks: u32,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DockedState {
    pub station_id: NetworkId,
    pub docked_at_tick: u64,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize, Default)]
pub struct ExtractionBeam {
    pub active: bool,
    pub target: Option<NetworkId>,
    pub extraction_range: f32,
    pub base_extraction_rate: u16,
    #[serde(skip)]
    pub last_seen_input_tick: Option<u64>,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DataStore {
    pub payload_count: u16,
    pub capacity: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Resource {
    pub payload_remaining: u16,
    pub total_capacity: u16,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolComponent(pub aetheris_protocol::types::Tool);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PriorityPoolComponent(pub aetheris_protocol::types::PriorityPool);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntegrityPoolComponent(pub aetheris_protocol::types::IntegrityPool);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DataDropComponent(pub aetheris_protocol::types::DataDrop);

impl_component_serde!(TransformComponent);
impl_component_serde!(Velocity);
impl_component_serde!(AgentPropertiesComponent);
impl_component_serde!(AgentConfiguration);
impl_component_serde!(AgentKindComponent);
impl_component_serde!(PlayerName);
impl_component_serde!(FactionTag);
impl_component_serde!(ResourceIntegrity);
impl_component_serde!(ResourceYield);
impl_component_serde!(DataDrop);
impl_component_serde!(Station);
impl_component_serde!(ZoneGate);
impl_component_serde!(BeamMarker);
impl_component_serde!(DockedState);
impl_component_serde!(ExtractionBeam);
impl_component_serde!(DataStore);
impl_component_serde!(Resource);
impl_component_serde!(ToolComponent);
impl_component_serde!(PriorityPoolComponent);
impl_component_serde!(IntegrityPoolComponent);
impl_component_serde!(DataDropComponent);

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceDefinitionComponent(pub WorkspaceDefinition);
impl_component_serde!(WorkspaceDefinitionComponent);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceBoundsComponent(pub WorkspaceBounds);
impl_component_serde!(WorkspaceBoundsComponent);

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceMembershipComponent(pub WorkspaceMembership);
impl_component_serde!(WorkspaceMembershipComponent);

// ──────────────────────────────────────────────
// Server-Only Components (M1020 §3.4)
// ──────────────────────────────────────────────

#[derive(Component, Debug, Clone)]
pub struct Budget {
    pub tokens: u64,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct PhysicsBody {
    pub base_mass: f32,
    pub thrust_force: f32,
    pub max_velocity: f32,
    pub turn_rate: f32,
    pub drag: f32,
    pub mass_per_payload: f32,
}

#[derive(Component, Debug, Clone)]
pub struct ToolSlot {
    pub tool_type: ToolId,
    pub cooldown_ticks: u16,
    pub current_cooldown: u16,
}

#[derive(Component, Debug, Clone)]
pub struct ToolCooldown {
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
    pub tool_range: f32,
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
pub struct ResourceRespawn {
    pub delay_ticks: u32,
    pub remaining: u32,
    pub x: f32,
    pub y: f32,
    pub total_capacity: u16,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct PriorityRegenTimer {
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
        aetheris_protocol::events::WireEvent,
    )>,
}

/// Resource used to optimize Stage 4 entity extraction by grouping entities by Workspace.
#[derive(bevy_ecs::prelude::Resource, Debug, Clone, Default)]
pub struct WorkspaceIndex {
    pub memberships:
        std::collections::HashMap<NetworkId, std::collections::HashSet<bevy_ecs::prelude::Entity>>,
    pub client_workspaces: std::collections::HashMap<ClientId, NetworkId>,
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

/// Marker: this entity is the authoritative session agent for a client.
///
/// Only set on the entity spawned via `StartSession` / `spawn_session_agent`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct SessionAgent;

/// Marker: this entity is a training target.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct TrainingTarget;

/// Marker: this entity is controlled by AI.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AiControlled;
