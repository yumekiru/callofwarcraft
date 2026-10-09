//! Reads the other engine's published state.
//!
//! CoDCraft runs two processes: `iw4l.exe` owns Modern Warfare 2, and this client owns World of
//! Warcraft. The guest writes a small block of player state each frame and this module reads it,
//! which is the whole of the passthrough for now.
//!
//! The block is the guest's format, not this module's: `MAGIC`, the version and the per-player
//! stride are fixed by the other side and must be matched exactly. A block that is too short, the
//! wrong magic or a version this reader does not know is *refused* rather than reinterpreted,
//! because a misread position is worse than no position.
//!
//! Nothing here is required for the client to run. With no `CODCRAFT_STATE` in the environment the
//! module reports no guest and costs one resource.

use std::{
    collections::{HashMap, VecDeque},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
mod combat_math;
mod damage_direction;
mod effects;
mod input_writer;
mod profile;
pub(crate) mod gear;
mod grenades;
pub(crate) mod predator;
mod helicopter;
mod sentry;
mod kobold_pose;
mod ragdoll;
mod shot_math;
mod soldiers;
mod soldier_lifecycle;
pub(crate) use soldiers::NativeSoldierAnchor;
mod tracers;

use crate::creature_anim::wrap_pi;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

#[path = "codcraft_aim.rs"]
mod aim;

/// The guest's first four bytes.
const MAGIC: &[u8; 4] = b"CODC";
/// The block version this reader understands.
const VERSION: u32 = 3;
/// One player: id, origin, velocity, viewangles, pm_flags, weapon, health, max_health, attack.
const PLAYER_WIRE: usize = 4 + (3 + 3 + 3) * 4 + 4 + 4 + 4 + 4 + 4 + 4;
/// magic + version + count + tick.
const HEADER_WIRE: usize = 4 + 4 + 4 + 4;

const INPUT_MAGIC: &[u8; 4] = b"CCIN";
const INPUT_VERSION: u32 = 1;
const INPUT_WIRE: usize = 44;
const INPUT_FORWARD: u32 = 1 << 0;
const INPUT_BACK: u32 = 1 << 1;
const INPUT_LEFT: u32 = 1 << 2;
const INPUT_RIGHT: u32 = 1 << 3;
const INPUT_JUMP: u32 = 1 << 4;
pub(crate) const INPUT_SPRINT: u32 = 1 << 5;
const INPUT_FIRE: u32 = 1 << 6;
const INPUT_AIM: u32 = 1 << 7;
const INPUT_RELOAD: u32 = 1 << 8;
const INPUT_CROUCH: u32 = 1 << 9;
const INPUT_PRONE: u32 = 1 << 10;
const INPUT_FRAG: u32 = 1 << 11;

/// Host-to-guest controls. Mouse motion is cumulative so the guest can recover motion even when
/// its render rate briefly misses a host frame.
#[derive(Resource, Default)]
pub(crate) struct GuestInputPublisher {
    writer: input_writer::Writer,
    path: Option<PathBuf>,
    sequence: u64,
    mouse_total: [f64; 2],
    fire_latch_until: f32,
    pub(crate) buttons: u32,
    alt_interact: bool,
    predator_control: bool,
    aim: Option<aim::AimSample>,
}

impl GuestInputPublisher {
    /// When configured, CoD owns movement and aim; Benilla decodes the same WASD as an FPS.
    pub(crate) fn owns_gameplay_controls(&self) -> bool {
        self.path.is_some()
    }

    /// Left Alt toggles Warcraft cursor interaction until pressed again.
    pub(crate) fn allows_world_interaction(&self) -> bool {
        self.owns_gameplay_controls() && self.alt_interact && !self.predator_control
    }
    pub(crate) fn controls_predator(&self) -> bool { self.predator_control }
}

/// Whether the local Warcraft attack seams must stay disabled because the passthrough bridge is
/// active. This is usable from non-system helpers such as `start_attack_local`.
pub(crate) fn passthrough_enabled() -> bool {
    std::env::var_os("CODCRAFT_INPUT").is_some_and(|value| !value.is_empty())
}

/// The small amount of host-side combat state needed to correlate the custom bullet request with
/// the server's normal `SMSG_ATTACKERSTATEUPDATE` response. The response is the authority for a
/// hit marker; the request itself is not, because the server can still reject a dead, hostile or
/// out-of-range target.
#[derive(Resource, Default)]
pub(crate) struct CodcraftCombatState {
    pending: VecDeque<PendingCodcraftShot>,
    grenade_pending: HashMap<u64, f32>,
    confirmed_bullet_hits: HashMap<u64, f32>,
    requested_loot: HashMap<u64, f32>,
    pub(crate) marker_until: f32,
    attack_stop_sent: bool,
    pub(crate) incoming_hits: VecDeque<(u64, f32)>,
    pub(crate) rifle_attackers: std::collections::HashSet<u64>,
    pub(crate) remote_player_shots: VecDeque<(u64, u64, f32)>,
    pub(crate) remote_player_fire_until: HashMap<u64, f32>,
}

struct PendingCodcraftShot {
    guid: u64,
    expires_at: f32,
}

impl CodcraftCombatState {
    const PENDING_TIMEOUT: f32 = 1.0;
    const CONFIRMED_HIT_LIFETIME: f32 = 120.0;
    const LOOT_REQUEST_LIFETIME: f32 = 3.0;

    pub(crate) fn queue(&mut self, guid: u64, now: f32) {
        self.pending.retain(|shot| shot.expires_at > now);
        self.pending.push_back(PendingCodcraftShot {
            guid,
            expires_at: now + Self::PENDING_TIMEOUT,
        });
    }

    pub(crate) fn take_for_response(&mut self, guid: u64, now: f32) -> Option<u64> {
        self.grenade_pending.retain(|_, deadline| *deadline > now);
        if self.grenade_pending.remove(&guid).is_some() {
            return Some(guid);
        }
        self.pending.retain(|shot| shot.expires_at > now);
        if let Some(index) = self.pending.iter().position(|shot| shot.guid == guid) {
            return self.pending.remove(index).map(|shot| shot.guid);
        }
        // vmangos can serialize the victim as a different GUID form from the object GUID that
        // was selected locally.  The response is still unambiguously ours when it is the next
        // attacker-state packet after a queued custom bullet, so consume the oldest pending shot
        // rather than dropping the hit marker and falling back into melee handling.
        self.pending.pop_front().map(|shot| shot.guid)
    }

    pub(crate) fn confirm_bullet_hit(&mut self, guid: u64, now: f32) {
        self.confirmed_bullet_hits
            .insert(guid, now + Self::CONFIRMED_HIT_LIFETIME);
    }

    fn queue_grenade(&mut self, guid: u64, now: f32) {
        self.grenade_pending
            .insert(guid, now + Self::PENDING_TIMEOUT);
    }

    fn take_recent_kill_candidate(&mut self, guid: u64, now: f32) -> bool {
        self.confirmed_bullet_hits
            .retain(|_, expires| *expires > now);
        self.confirmed_bullet_hits.contains_key(&guid)
    }

    fn claim_auto_loot(&mut self, guid: u64, now: f32) -> bool {
        self.requested_loot.retain(|_, expires| *expires > now);
        if self.requested_loot.contains_key(&guid) {
            return false;
        }
        self.requested_loot
            .insert(guid, now + Self::LOOT_REQUEST_LIFETIME);
        true
    }

    pub(crate) fn reset_attack_stop(&mut self) {
        self.attack_stop_sent = false;
    }

    pub(crate) fn should_send_attack_stop(&mut self) -> bool {
        if self.attack_stop_sent {
            false
        } else {
            self.attack_stop_sent = true;
            true
        }
    }
}

/// A confirmed custom bullet hit. The HUD consumes this instead of guessing from the trigger
/// press, so misses and server-side refusals do not flash a false marker.
#[derive(Message, Clone, Copy)]
pub(crate) struct CodcraftHitMarker;

/// A confirmed hit. The sound layer forwards this to iw4L, which plays the native MW2
/// `MP_hit_alert` alias; it is deliberately not a Warcraft melee sound.
#[derive(Message, Clone, Copy)]
pub(crate) struct CodcraftBulletImpact;

/// Damage text for a confirmed CoD bullet. It is deliberately separate from `SwingImpact`, whose
/// consumers also animate and audio-render a Warcraft melee swing.
#[derive(Message, Clone, Copy)]
pub(crate) struct CodcraftDamageText {
    pub(crate) anchor: Entity,
    pub(crate) hit_info: u32,
    pub(crate) victim_state: u32,
    pub(crate) damage: u32,
}

#[derive(Component)]
struct CodcraftHudRoot;

#[derive(Component)]
struct CodcraftCrosshair;

#[derive(Component)]
struct CodcraftHitMarkerVisual;

/// Spawn the screen-space CoD reticle above the normal world/UI passes. It has no buttons or
/// interaction handlers, and `FocusPolicy::Pass` keeps it transparent to Warcraft input.
fn setup_codcraft_hud(mut commands: Commands) {
    commands
        .spawn((
            CodcraftHudRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            bevy::ui::FocusPolicy::Pass,
            GlobalZIndex(1050),
        ))
        .with_children(|ui| {
            ui.spawn((
                CodcraftCrosshair,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Px(0.0),
                    height: Val::Px(0.0),
                    ..default()
                },
                bevy::ui::FocusPolicy::Pass,
                Transform::default(),
                Visibility::Hidden,
            ))
            .with_children(|crosshair| {
                let segment = |left, top, width, height| {
                    (
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(left),
                            top: Val::Px(top),
                            width: Val::Px(width),
                            height: Val::Px(height),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.9)),
                    )
                };
                crosshair.spawn(segment(-1.0, -14.0, 2.0, 8.0));
                crosshair.spawn(segment(-1.0, 6.0, 2.0, 8.0));
                crosshair.spawn(segment(-14.0, -1.0, 8.0, 2.0));
                crosshair.spawn(segment(6.0, -1.0, 8.0, 2.0));
            });

            ui.spawn((
                CodcraftHitMarkerVisual,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Px(0.0),
                    height: Val::Px(0.0),
                    ..default()
                },
                bevy::ui::FocusPolicy::Pass,
                Transform::default(),
                Visibility::Hidden,
            ))
            .with_children(|marker| {
                let stroke = |rotation| {
                    (
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(-1.0),
                            top: Val::Px(-17.0),
                            width: Val::Px(2.0),
                            height: Val::Px(12.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.95)),
                        Transform::from_rotation(Quat::from_rotation_z(rotation)),
                    )
                };
                marker.spawn(stroke(45.0_f32.to_radians()));
                marker.spawn(stroke(-45.0_f32.to_radians()));
            });
        });
}

fn update_codcraft_hud(
    time: Res<Time>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    input: Res<GuestInputPublisher>,
    view: Res<GuestView>,
    mouse: Res<ButtonInput<MouseButton>>,
    typing: Res<crate::ui_script::UiKeyboardCapture>,
    mut combat: ResMut<CodcraftCombatState>,
    mut markers: MessageReader<CodcraftHitMarker>,
    mut visibility: ParamSet<(
        Query<(&mut Visibility, &mut Transform), With<CodcraftCrosshair>>,
        Query<(&mut Visibility, &mut Transform), With<CodcraftHitMarkerVisual>>,
    )>,
) {
    let now = time.elapsed_secs();
    if markers.read().next().is_some() {
        combat.marker_until = now + 0.16;
    }
    let live = world_live.0 && input.owns_gameplay_controls() && view.shown;
    let hip_fire = live && (input.predator_control || !mouse.pressed(MouseButton::Right)) && !typing.typing;
    let marker_remaining = (combat.marker_until - now).clamp(0.0, 0.16);
    let marker = live && marker_remaining > 0.0;
    let marker_progress = 1.0 - marker_remaining / 0.16;
    if let Ok((mut visibility, mut transform)) = visibility.p0().single_mut() {
        *visibility = if hip_fire {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        let pulse = if marker && hip_fire {
            1.0 + 0.45 * (1.0 - marker_progress)
        } else {
            1.0
        };
        transform.scale = Vec3::splat(pulse);
    }
    if let Ok((mut visibility, mut transform)) = visibility.p1().single_mut() {
        *visibility = if marker {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        transform.scale = Vec3::splat(0.75 + 0.55 * marker_progress);
    }
}

fn configure_input(mut input: ResMut<GuestInputPublisher>) {
    input.path = std::env::var_os("CODCRAFT_INPUT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if let Some(path) = &input.path {
        info!("CoDCraft: input bridge file {}", path.display());
    }
}

/// Publish Warcraft-window input for the minimized MW2 client. The tiny file uses an invalidated
/// header while being rewritten, so a concurrent reader either sees a complete packet or skips it.
fn publish_guest_input(
    time: Res<Time>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    over_ui: Res<crate::ui_script::PointerOverUi>,
    typing: Res<crate::ui_script::UiKeyboardCapture>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut input: ResMut<GuestInputPublisher>,
    gear: Res<gear::GearState>,
) {
    let _work_scope = profile::scope("codcraft.rs:publish_guest_input");
    let Some(path) = input.path.clone() else {
        return;
    };
    let focused = windows.single().is_ok_and(|window| window.focused);
    let active = world_live.0 && focused;
    let now = time.elapsed_secs();
    let keyboard_enabled = active && !typing.typing;
    if !active {
        input.alt_interact = false;
    } else if !input.predator_control && !typing.typing && keys.just_pressed(KeyCode::AltLeft) {
        input.alt_interact = !input.alt_interact;
    }
    let mouse_gameplay = active && !typing.typing && (input.predator_control || (!input.alt_interact && !over_ui.0));
    let armed = !gear::enabled() || gear.code != 0;
    let mut buttons = 0;
    let mut set = |condition, bit| {
        if condition {
            buttons |= bit;
        }
    };
    set(
        keyboard_enabled && keys.pressed(KeyCode::KeyW),
        INPUT_FORWARD,
    );
    set(keyboard_enabled && keys.pressed(KeyCode::KeyS), INPUT_BACK);
    set(keyboard_enabled && keys.pressed(KeyCode::KeyA), INPUT_LEFT);
    set(keyboard_enabled && keys.pressed(KeyCode::KeyD), INPUT_RIGHT);
    set(keyboard_enabled && keys.pressed(KeyCode::Space), INPUT_JUMP);
    set(
        keyboard_enabled && keys.pressed(KeyCode::ShiftLeft),
        INPUT_SPRINT,
    );
    set(
        mouse_gameplay && armed && mouse.pressed(MouseButton::Left),
        INPUT_FIRE,
    );
    if mouse_gameplay && armed && mouse.just_pressed(MouseButton::Left) {
        input.fire_latch_until = now + 0.08;
    }
    set(
        mouse_gameplay && armed && input.fire_latch_until > now,
        INPUT_FIRE,
    );
    set(
        mouse_gameplay && mouse.pressed(MouseButton::Right),
        INPUT_AIM,
    );
    set(
        keyboard_enabled && keys.pressed(KeyCode::KeyR),
        INPUT_RELOAD,
    );
    set(
        keyboard_enabled && keys.pressed(KeyCode::KeyC),
        INPUT_CROUCH,
    );
    set(keyboard_enabled && keys.pressed(KeyCode::KeyZ), INPUT_PRONE);
    set(keyboard_enabled && keys.pressed(KeyCode::KeyG), INPUT_FRAG);
    if input.predator_control { buttons &= INPUT_FIRE; }
    input.buttons = buttons;

    // CoD owns the view direction: do not require WoW's own mouse-look drag gesture. Preserve UI
    // usability by stopping aim updates over a hit-tested UI element or while typing.
    if mouse_gameplay {
        input.mouse_total[0] += f64::from(motion.delta.x);
        input.mouse_total[1] += f64::from(motion.delta.y);
    }
    input.sequence = input.sequence.wrapping_add(1);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as u64)
        .unwrap_or(0);
    let mut payload = Vec::with_capacity(INPUT_WIRE - 16);
    payload.extend_from_slice(&stamp.to_le_bytes());
    payload.extend_from_slice(&buttons.to_le_bytes());
    payload.extend_from_slice(&input.mouse_total[0].to_le_bytes());
    payload.extend_from_slice(&input.mouse_total[1].to_le_bytes());

    let header = || {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(INPUT_MAGIC);
        bytes.extend_from_slice(&INPUT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&input.sequence.to_le_bytes());
        bytes
    };
    let mut packet = header();
    packet.extend_from_slice(&payload);
    input.writer.publish(path, packet);
}

/// Keep the normal Warcraft camera depth buffer for scene/weapon occlusion, but relax its near
/// plane just for ADS. The rear of the first-person gun can otherwise cross the 0.1 yd clip plane
/// when the CoD viewmodel shifts closer during aim; restoring the user's original nearclip on
/// release avoids the depth-precision cost outside ADS.
#[derive(Resource, Default)]
struct CodcraftNearClip {
    original: Option<f32>,
}

fn tune_ads_near_clip(
    input: Res<GuestInputPublisher>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    mut view: ResMut<benilla_world::view::ViewDistance>,
    mut state: ResMut<CodcraftNearClip>,
) {
    let aiming = world_live.0 && input.owns_gameplay_controls() && input.buttons & INPUT_AIM != 0;
    if aiming {
        state.original.get_or_insert(view.nearclip);
        let ads_nearclip = *benilla_world::view::NEARCLIP_RANGE.start();
        // `ViewDistance` is shared with scene visibility and particle draw-set systems. Calling
        // through `ResMut` marks the resource changed even when the value is identical, which
        // forces their whole-scene invalidation path for every ADS frame. Touch it only on the
        // transition into ADS (or a real near-clip change).
        if view.nearclip != ads_nearclip {
            view.set_nearclip(ads_nearclip);
        }
    } else if let Some(original) = state.original.take() {
        if view.nearclip != original {
            view.set_nearclip(original);
        }
    }
}

/// One player, as the guest published it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GuestPlayer {
    pub id: u32,
    /// Guest units: inches, Z-up. Not benilla's yards and Y-up.
    pub origin: [f32; 3],
    pub velocity: [f32; 3],
    /// Guest degrees.
    pub viewangles: [f32; 3],
    pub pm_flags: u32,
    pub weapon: u32,
    pub health: i32,
    pub max_health: i32,
    /// True while the guest's local attack button is held.
    pub attack: bool,
    /// Advances only after MW2 accepts and emits an actual weapon shot.
    pub shot_sequence: u32,
}

/// What the guest said this frame.
#[derive(Debug, Default)]
pub struct GuestState {
    /// The guest's simulation tick. Changes every frame it is running.
    pub tick: u32,
    pub players: Vec<GuestPlayer>,
    /// When this was read, so a stale file is distinguishable from a guest that has stopped.
    pub read_at: f32,
    /// Set when a block was present but unreadable, which is a fault rather than an absence.
    pub error: Option<String>,
}

/// The guest's block, polled once a frame.
///
/// The path is read once. A guest that is not running leaves the file stale, which is why
/// [`GuestState::read_at`] travels with the data: the host can tell "the guest is idle" from
/// "the guest is gone" without either looking like a frozen player.
#[derive(Resource, Default)]
pub struct GuestLink {
    path: Option<PathBuf>,
    state: GuestState,
    reported: bool,
}

impl GuestLink {
    pub fn state(&self) -> &GuestState {
        &self.state
    }
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Read the block, or explain why not.
fn read(path: &std::path::Path, now: f32) -> GuestState {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return GuestState {
                read_at: now,
                error: Some(e.to_string()),
                ..Default::default()
            };
        }
    };
    if bytes.len() < HEADER_WIRE {
        return GuestState {
            read_at: now,
            error: Some(format!("{} bytes is shorter than a header", bytes.len())),
            ..Default::default()
        };
    }
    if &bytes[0..4] != MAGIC {
        return GuestState {
            read_at: now,
            error: Some("not a CoDCraft state block".to_owned()),
            ..Default::default()
        };
    }
    let version = u32_at(&bytes, 4);
    if version != VERSION {
        return GuestState {
            read_at: now,
            error: Some(format!(
                "block version {version}, this reader speaks {VERSION}"
            )),
            ..Default::default()
        };
    }
    let count = u32_at(&bytes, 8) as usize;
    let tick = u32_at(&bytes, 12);
    // The count is bounded by the bytes actually present, so a corrupt or truncated count cannot
    // make this read past the end.
    let fits = count.saturating_mul(PLAYER_WIRE);
    if HEADER_WIRE + fits > bytes.len() {
        return GuestState {
            read_at: now,
            error: Some(format!(
                "claims {count} players, which needs {} bytes, in {}",
                HEADER_WIRE + fits,
                bytes.len()
            )),
            ..Default::default()
        };
    }

    let mut players = Vec::with_capacity(count);
    for i in 0..count {
        let b = HEADER_WIRE + i * PLAYER_WIRE;
        players.push(GuestPlayer {
            id: u32_at(&bytes, b),
            origin: [
                f32_at(&bytes, b + 4),
                f32_at(&bytes, b + 8),
                f32_at(&bytes, b + 12),
            ],
            velocity: [
                f32_at(&bytes, b + 16),
                f32_at(&bytes, b + 20),
                f32_at(&bytes, b + 24),
            ],
            viewangles: [
                f32_at(&bytes, b + 28),
                f32_at(&bytes, b + 32),
                f32_at(&bytes, b + 36),
            ],
            pm_flags: u32_at(&bytes, b + 40),
            weapon: u32_at(&bytes, b + 44),
            health: i32_at(&bytes, b + 48),
            max_health: i32_at(&bytes, b + 52),
            attack: u32_at(&bytes, b + 56) != 0,
            shot_sequence: u32_at(&bytes, b + 60),
        });
    }
    GuestState {
        tick,
        players,
        read_at: now,
        error: None,
    }
}

/// Poll the guest once a frame.
pub fn poll(time: Res<Time>, mut link: ResMut<GuestLink>) {
    let Some(path) = link.path.clone() else {
        return;
    };
    let mut next = read(&path, time.elapsed_secs());
    // The guest invalidates its header while publishing the next player-state block. Treat a
    // concurrent read of that short write as a missed frame; a real disconnect still expires.
    if next.error.is_some()
        && link.state.error.is_none()
        && time.elapsed_secs() - link.state.read_at <= 0.5
    {
        return;
    }
    if next.error.is_none() && next.tick == link.state.tick {
        next.read_at = link.state.read_at;
    }
    link.state = next;
    if !link.reported {
        link.reported = true;
        match &link.state.error {
            None => info!("CoDCraft: guest state at {}", path.display()),
            Some(e) => warn!("CoDCraft: guest state at {}: {e}", path.display()),
        }
    }
}

/// Read the block once at boot, before anything else is set up.
pub fn configure(mut link: ResMut<GuestLink>) {
    info!(
        "CoDCraft: modern lighting preview enabled={}",
        std::env::var("CODCRAFT_REALISTIC_LIGHTING").as_deref() == Ok("1")
    );
    link.path = std::env::var_os("CODCRAFT_STATE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    if let Some(p) = &link.path {
        info!("CoDCraft: guest state file {}", p.display());
    }
}

/// The passthrough link, and nothing else. It reads the other engine's state; it does not move this
/// client's player.
pub struct CodcraftPlugin;

/// Maps the guest's MW2 coordinates onto the host's current WoW position. The two games do not
/// share a world origin, so the first valid packet establishes a local anchor and subsequent guest
/// movement is carried across at the default 36-inch-per-yard scale. `CODCRAFT_SCALE` can override
/// that scale for a different asset's unit convention.
#[derive(Resource)]
struct CodcraftPoseMap {
    guest_anchor: Option<Vec3>,
    host_anchor: Option<Vec3>,
    yaw_offset: Option<f32>,
    scale: f32,
    aim_assist: bool,
    reported: bool,
    last_shot: Option<u32>,
}

impl Default for CodcraftPoseMap {
    fn default() -> Self {
        let scale = std::env::var("CODCRAFT_SCALE")
            .ok()
            .and_then(|raw| raw.parse::<f32>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0 / 36.0);
        Self {
            guest_anchor: None,
            host_anchor: None,
            yaw_offset: None,
            scale,
            aim_assist: std::env::var("CODCRAFT_AIM_ASSIST")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE"))
                // First-pass bulletfire should still hit the nearest streamed mob when the
                // interpolated ray misses by a few pixels; this keeps target selection usable
                // while the two engines have independent camera/pose sampling.
                .unwrap_or(true),
            reported: false,
            last_shot: None,
        }
    }
}

/// Apply the MW2 player's interpolated pose to the host's player puppet. This is deliberately a
/// one-way movement link: MW2 is authoritative for the player while WoW remains authoritative for
/// its world, collision and NPC state.
#[derive(Default)]
struct AimCadence {
    last_at: f32,
    last_angles: Option<[f32; 2]>,
    last_totals: [f64; 2],
    last_tick: u32,
    frames: u32,
    turns: u32,
    motion: u32,
    predicted: u32,
    snapshots: u32,
}

impl AimCadence {
    fn observe(&mut self, at: f32, angles: [f32; 2], totals: [f64; 2], tick: u32, predicted: bool) {
        static PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
        let Some(path) =
            PATH.get_or_init(|| std::env::var_os("CODCRAFT_AIM_TRACE").map(PathBuf::from))
        else {
            return;
        };
        self.frames += 1;
        self.turns += u32::from(self.last_angles.is_some_and(|last| last != angles));
        self.motion += u32::from(self.last_totals != totals);
        self.predicted += u32::from(predicted);
        self.snapshots += u32::from(self.last_tick != tick);
        self.last_angles = Some(angles);
        self.last_totals = totals;
        self.last_tick = tick;
        if at - self.last_at < 1.0 {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(
                file,
                "t={at:.2} frames={} mouse_frames={} camera_turn_frames={} predicted_frames={} authority_snapshots={}",
                self.frames, self.motion, self.turns, self.predicted, self.snapshots
            );
        }
        self.frames = 0;
        self.turns = 0;
        self.motion = 0;
        self.predicted = 0;
        self.snapshots = 0;
        self.last_at = at;
    }
}

fn apply_guest_player(
    time: Res<Time>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    link: Res<GuestLink>,
    mut map: ResMut<CodcraftPoseMap>,
    mut player: ResMut<crate::player::Player>,
    mut camera: Query<
        (&mut crate::player::camera::FlyCam, &Transform),
        With<benilla_world::view::WorldCamera>,
    >,
    mut camera_control: ResMut<crate::player::camera::CameraControl>,
    mut input: ResMut<GuestInputPublisher>,
    units: Query<
        (&crate::net::Guid, &crate::net::NetEntity, &Transform),
        (
            Without<crate::net::SelfPlayer>,
            Without<crate::net::Embodied>,
        ),
    >,
    self_attack: Query<Has<crate::creature_anim::Engaged>, With<crate::net::SelfPlayer>>,
    net: Option<Res<crate::net::NetCommands>>,
    mut combat: ResMut<CodcraftCombatState>,
    mut cadence: Local<AimCadence>,
    mut effects: ResMut<effects::Effects>,
) {
    let _work_scope = profile::scope("codcraft.rs:apply_guest_player");
    if input.predator_control { return; }
    let _work_scope = profile::scope("codcraft.rs:poll");
    if !world_live.0 {
        return;
    }
    if time.elapsed_secs() - link.state().read_at > 0.5 {
        return;
    }
    let Some(guest) = link
        .state()
        .players
        .iter()
        .find(|p| p.id == 0)
        .or_else(|| link.state().players.first())
    else {
        return;
    };
    if !guest.origin.iter().all(|v| v.is_finite())
        || !guest.viewangles.iter().all(|v| v.is_finite())
    {
        return;
    }
    // The guest publishes a player record before its match spawn has populated the pose. Do not
    // consume that zeroed record as the cross-engine anchor; wait for the live player snapshot.
    if guest.health <= 0 || guest.max_health <= 0 {
        return;
    }

    let guest_pos = Vec3::from_array(guest.origin);
    let guest_yaw = guest.viewangles[1].to_radians();
    if map.guest_anchor.is_none() {
        if !player.active {
            return;
        }
        map.guest_anchor = Some(guest_pos);
        map.host_anchor = Some(player.pos);
        map.yaw_offset = Some(wrap_pi(player.face_yaw() - guest_yaw));
        if !map.reported {
            map.reported = true;
            info!(
                "CoDCraft: player link anchored guest {:?} to host {:?}, scale {:.6}",
                guest.origin, player.pos, map.scale
            );
        }
    }

    // Use MW2's sampled client view plus unacknowledged local mouse counts. Authority snapshots
    // are 20 Hz; using their angles directly makes a 144 Hz world turn in 20 Hz steps.
    if let Some(sample) = input
        .path
        .as_ref()
        .and_then(|path| std::fs::read(path.with_extension("aim")).ok())
        .and_then(|bytes| aim::decode(&bytes))
    {
        input.aim = Some(sample);
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let predicted = input
        .aim
        .and_then(|sample| aim::predict(sample, input.mouse_total, input.sequence, stamp));
    // MW2 pitch is positive-down, while the Bevy rig is positive-up.
    let (yaw, pitch, target) = if let Ok((mut cam, transform)) = camera.single_mut() {
        let yaw = wrap_pi(predicted.map_or(guest_yaw, |a| a[0]) + map.yaw_offset.unwrap_or(0.0));
        let pitch = -predicted.map_or(guest.viewangles[0].to_radians(), |a| a[1]);
        cam.park(yaw, pitch);
        (yaw, pitch, transform.translation)
    } else {
        (player.face_yaw(), 0.0, player.pos + Vec3::Y * 1.6)
    };
    cadence.observe(
        time.elapsed_secs(),
        [yaw, pitch],
        input.mouse_total,
        link.state.tick,
        predicted.is_some(),
    );
    // The same mapped CoD heading drives both the Benilla camera and its movement basis.
    let current_yaw = player.face_yaw();
    player.turn_aim(wrap_pi(yaw - current_yaw));
    player.aim_pitch(pitch);
    camera_control.codcraft_first_person(guest.pm_flags);
    let shot = map
        .last_shot
        .is_some_and(|last| last != guest.shot_sequence);
    map.last_shot = Some(guest.shot_sequence);
    if let Some(net) = net {
        // A CoD trigger is never a Warcraft melee command. If a world click or a previous
        // session left the normal auto-attack latch engaged, stop it once and let the server echo
        // clear the animation state. The custom bullet packet below is the only combat request.
        if self_attack.single().unwrap_or(false) {
            if combat.should_send_attack_stop() {
                let _ = net.0.send(crate::net::ClientCommand::AttackStop);
            }
        } else {
            combat.reset_attack_stop();
        }
        if shot {
            let aimed_target = pick_guest_target(target, yaw, pitch, &units);
            let target_guid = aimed_target.or_else(|| {
                map.aim_assist
                    .then(|| pick_nearest_guest_target(target, &units))
                    .flatten()
            });
            if let Some(target_guid) = target_guid {
                // A unit impact terminates this bullet. The FX bridge must not
                // independently raycast through that unit into scenery behind it.
                let direction = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
                if let Some((_, _, transform)) =
                    units.iter().find(|(guid, _, _)| guid.0 == target_guid)
                {
                    let offset = transform.translation + Vec3::Y - target;
                    let distance = offset.dot(direction);
                    // Combat's generous aim-assist cone is not a physical impact.
                    if distance > 0.0
                        && (offset - direction * distance).length_squared() <= 0.85 * 0.85
                    {
                        effects.unit_impact(guest.shot_sequence, (distance - 0.5).max(0.0));
                    }
                }
                // Queue before sending: the server can answer on the very next network turn.
                combat.queue(target_guid, time.elapsed_secs());
                let _ = net
                    .0
                    .send(crate::net::ClientCommand::CodcraftBullet { guid: target_guid });
                if aimed_target.is_some() {
                    info!("CoDCraft: MW2 trigger down; Warcraft target {target_guid:#x}");
                } else {
                    info!(
                        "CoDCraft: MW2 trigger down; Warcraft target {target_guid:#x} (aim assist)"
                    );
                }
            } else {
                let direction = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
                let mut unit_count = 0usize;
                let mut nearest: Option<(u64, f32, f32)> = None;
                for (guid, entity, transform) in &units {
                    if entity.kind != benilla_protocol::EntityKind::Unit {
                        continue;
                    }
                    unit_count += 1;
                    let to_unit = transform.translation + Vec3::Y - target;
                    let distance = to_unit.length();
                    let angle = if distance > f32::EPSILON {
                        direction.angle_between(to_unit).to_degrees()
                    } else {
                        0.0
                    };
                    if nearest.is_none_or(|(_, best_distance, _)| distance < best_distance) {
                        nearest = Some((guid.0, distance, angle));
                    }
                }
                warn!(
                    "CoDCraft: MW2 trigger down but no Warcraft unit is in the aim cone; \
                     streamed_units={unit_count} nearest={nearest:?}"
                );
            }
        }
    }
}

/// Pick the nearest streamed creature in the guest-mapped aim cone. The server remains the
/// authority for range, facing, health and damage; this only turns the crosshair into a target.
fn pick_guest_target(
    origin: Vec3,
    yaw: f32,
    pitch: f32,
    units: &Query<
        (&crate::net::Guid, &crate::net::NetEntity, &Transform),
        (
            Without<crate::net::SelfPlayer>,
            Without<crate::net::Embodied>,
        ),
    >,
) -> Option<u64> {
    let direction = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
    let mut best: Option<(f32, u64)> = None;
    for (guid, entity, transform) in units {
        if entity.kind != benilla_protocol::EntityKind::Unit {
            continue;
        }
        let to_target = transform.translation + Vec3::Y - origin;
        let along = to_target.dot(direction);
        if !(0.0..=80.0).contains(&along) {
            continue;
        }
        let lateral_sq = (to_target - direction * along).length_squared();
        if lateral_sq > 4.0 * 4.0 {
            continue;
        }
        let score = lateral_sq * 4.0 + along * 0.001;
        if best.is_none_or(|(best_score, _)| score < best_score) {
            best = Some((score, guid.0));
        }
    }
    best.map(|(_, guid)| guid)
}

fn pick_nearest_guest_target(
    origin: Vec3,
    units: &Query<
        (&crate::net::Guid, &crate::net::NetEntity, &Transform),
        (
            Without<crate::net::SelfPlayer>,
            Without<crate::net::Embodied>,
        ),
    >,
) -> Option<u64> {
    units
        .iter()
        .filter(|(_, entity, _)| entity.kind == benilla_protocol::EntityKind::Unit)
        .filter_map(|(guid, _, transform)| {
            let distance = (transform.translation + Vec3::Y - origin).length();
            (distance <= 80.0).then_some((distance, guid.0))
        })
        .min_by(|(a, _), (b, _)| a.total_cmp(b))
        .map(|(_, guid)| guid)
}

// ---- the live first-person model ---------------------------------------------------------------

fn le32(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    v.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(v)
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(v)
}

/// Whether the streamed 3D arms/weapon are shown. `Q` toggles them without changing either game.
#[derive(Resource)]
pub struct GuestView {
    /// On: the guest viewmodel is rendered in Benilla's world camera. `Q` toggles it.
    pub shown: bool,
    error: Option<String>,
}

impl Default for GuestView {
    fn default() -> Self {
        Self {
            shown: true,
            error: None,
        }
    }
}

/// Static mesh and live pose paths. They derive from CODCRAFT_FRAME unless explicitly overridden.
#[derive(Resource)]
struct ViewmodelPaths {
    model: std::path::PathBuf,
    pose: std::path::PathBuf,
}

#[derive(Component)]
struct CodcraftViewmodel;

#[derive(Component)]
struct CodcraftKoboldWeapon;

const KOBOLD_MAX_RANGE: f32 = 28.0;

/// CoDCraft fork: one eligibility marker shared by weapon visuals, pose and guest AI.
#[derive(Component)]
pub(crate) struct RifleEnemy;

fn classify_rifle_enemies(
    mut commands: Commands,
    input: Res<GuestInputPublisher>,
    names: Res<crate::names::NameCache>,
    reactions: crate::target::ReactionInputs,
    mut combat: ResMut<CodcraftCombatState>,
    net: Option<Res<crate::net::NetCommands>>,
    own: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    mut units: Query<
        (
            Entity,
            &crate::net::Guid,
            &crate::net::NetEntity,
            &mut crate::net::ObjectStore,
            Option<&crate::entities::BoneAttach>,
            Option<&RifleEnemy>,
        ),
        Without<crate::net::SelfPlayer>,
    >,
) {
    let own = own.iter().next();
    combat.rifle_attackers.clear();
    for (entity, guid, kind, mut store, attach, marker) in &mut units {
        if kind.kind != benilla_protocol::EntityKind::Unit {
            continue;
        }
        let Some(entry) = benilla_protocol::guid::entry(guid.0) else {
            continue;
        };
        if let Some(net) = net.as_deref() {
            names.resolve_creature(entry, guid.0, net);
        }
        let hostile = shot_math::trial_enemy(entry)
            || crate::target::ring_reaction(
                reactions.factions.as_deref(),
                &reactions.reputations,
                Some(&store),
                own,
            ) < 4;
        let hands = attach.is_some_and(|a| {
            a.points
                .contains_key(&crate::entities::attach_id::HAND_RIGHT)
                && a.points
                    .contains_key(&crate::entities::attach_id::HAND_LEFT)
        });
        let eligible = input.owns_gameplay_controls()
            && shot_math::eligible_humanoid(
                names.creature_type(entry).unwrap_or(0),
                hostile,
                hands,
                store.0.unit_flags() & 8 != 0,
            );
        if eligible {
            combat.rifle_attackers.insert(guid.0);
        }
        if eligible != marker.is_some() {
            store.set_changed();
            if eligible {
                commands.entity(entity).insert(RifleEnemy);
            } else {
                commands.entity(entity).remove::<RifleEnemy>();
            }
        }
    }
}

/// Host observations and commands for the controller running in the guest process.
#[derive(Resource, Default)]
struct CodcraftKoboldAi {
    next_poll: f32,
    sequence: u32,
    consumed: u32,
    shots: HashMap<u64, (u32, f32)>,
    pending_shots: VecDeque<(u64, f32, Vec3)>,
    clear_shooters: Vec<u64>,
    engaged: HashMap<u64, f32>,
    cover: HashMap<u64, (Vec3, Vec3, f32)>,
    cover_scan: usize,
    next_cover_scan: f32,
    command_budget: shot_math::CommandBudget,
    command_cursor: usize,
}

/// Benilla-side mesh assets for one CoD viewmodel generation.
#[derive(Resource, Default)]
struct ViewmodelStage {
    fingerprint: Option<u64>,
    vertex_n: usize,
    entities: Vec<Entity>,
    meshes: Vec<Handle<Mesh>>,
    materials: Vec<Handle<benilla_assets::materials::WowModelMaterial>>,
    pose_transform: Option<Mat4>,
    bot_entities: HashMap<u64, Vec<Entity>>,
}

#[derive(Resource, Default)]
struct KoboldGunStage {
    fingerprint: Option<u64>,
    meshes: Vec<Handle<Mesh>>,
    materials: Vec<Handle<benilla_assets::materials::WowModelMaterial>>,
    next_check: f32,
    muzzle: Vec3,
    forward: Vec3,
    up: Vec3,
}

fn present_kobold_gun(
    mut commands: Commands,
    time: Res<Time>,
    paths: Option<Res<ViewmodelPaths>>,
    mut gun: ResMut<KoboldGunStage>,
    mut stage: ResMut<ViewmodelStage>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch: benilla_world::model_render::M2BatchMaterials,
    cameras: Query<Entity, With<benilla_world::view::WorldCamera>>,
) {
    let Some(paths) = paths else { return };
    if time.elapsed_secs() < gun.next_check {
        return;
    }
    gun.next_check = time.elapsed_secs() + 0.25;
    let Ok(pose) =
        read_packet(&paths.model.with_extension("codv"), POSE_MAGIC).and_then(|p| parse_pose(&p))
    else {
        return;
    };
    if !pose.visible {
        return;
    }
    if gun.fingerprint != Some(pose.fingerprint) {
        let Ok(model) = read_packet(&paths.model.with_extension("codw"), MODEL_MAGIC)
            .and_then(|p| parse_model(&p))
        else {
            return;
        };
        if model.fingerprint != pose.fingerprint || model.uvs.len() != pose.positions.len() {
            return;
        }
        let Ok(camera) = cameras.single() else { return };
        let Ok((entities, handles, materials)) = install_model(
            &mut commands,
            &mut images,
            &mut meshes,
            &mut batch,
            camera,
            &model,
        ) else {
            return;
        };
        for entity in entities {
            commands.entity(entity).despawn();
        }
        for (_, entities) in stage.bot_entities.drain() {
            for entity in entities {
                commands.entity(entity).despawn();
            }
        }
        gun.meshes = handles;
        gun.materials = materials;
        gun.fingerprint = Some(pose.fingerprint);
        gun.muzzle = pose.transform.w_axis.truncate();
        gun.forward = pose.transform.x_axis.truncate();
        gun.up = pose.transform.y_axis.truncate();
        info!(
            "CoDCraft: installed native world-weapon Kobold model, muzzle={:?}",
            gun.muzzle
        );
    }
    // A generation identifies topology/materials, not the animated vertex pose. Keep
    // receiving the native weapon's parts after installation instead of freezing its
    // first (potentially still-loading) frame forever. Grip-space removes FPV sway.
    for handle in &gun.meshes {
        if let Some(mesh) = meshes.get_mut(handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pose.positions.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, pose.normals.clone());
        }
    }
}

fn resolve_viewmodel_paths(mut commands: Commands) {
    if let Some(raw) = std::env::var_os("CODCRAFT_FRAME").filter(|v| !v.is_empty()) {
        let frame = std::path::PathBuf::from(raw);
        let model = std::env::var_os("CODCRAFT_MODEL")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| frame.with_extension("codm"));
        let pose = std::env::var_os("CODCRAFT_POSE")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| frame.with_extension("codp"));
        info!(
            "CoDCraft: reading live FPV model from {} and {}",
            model.display(),
            pose.display()
        );
        commands.insert_resource(ViewmodelPaths { model, pose });
    }
}

const MODEL_MAGIC: &[u8; 4] = b"CODM";
const POSE_MAGIC: &[u8; 4] = b"CODP";
const BRIDGE_VERSION: u32 = 2;
const BRIDGE_HEADER: usize = 16;

fn read_packet(path: &std::path::Path, magic: &[u8; 4]) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() < BRIDGE_HEADER || &bytes[..4] != magic {
        return Err("packet is missing or incomplete".to_owned());
    }
    let version = le32(&bytes, 4);
    if version != BRIDGE_VERSION {
        return Err(format!("wire version {version}, expected {BRIDGE_VERSION}"));
    }
    let payload_len =
        usize::try_from(le64(&bytes, 8)).map_err(|_| "wire payload is too large".to_owned())?;
    if bytes.len() != BRIDGE_HEADER.saturating_add(payload_len) {
        return Err("wire payload length does not match the packet".to_owned());
    }
    Ok(bytes[BRIDGE_HEADER..].to_vec())
}

struct WireReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> WireReader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(len).ok_or("wire offset overflow")?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or("truncated wire packet")?;
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(bytes.try_into().expect("eight bytes")))
    }

    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }

    fn finished(&self) -> bool {
        self.at == self.bytes.len()
    }
}

struct ModelMaterialWire {
    alpha: u32,
    two_sided: bool,
    alpha_cutoff: f32,
    texture_srgb: bool,
    texture: Option<(u32, u32, Vec<u8>)>,
    indices: Vec<u32>,
}

struct ModelWire {
    fingerprint: u64,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
    materials: Vec<ModelMaterialWire>,
}

fn parse_model(payload: &[u8]) -> Result<ModelWire, String> {
    let mut reader = WireReader {
        bytes: payload,
        at: 0,
    };
    let fingerprint = reader.u64()?;
    let vertex_n = reader.u32()? as usize;
    let material_n = reader.u32()? as usize;
    if vertex_n == 0 || vertex_n > 1_000_000 || material_n > 4096 {
        return Err("implausible FPV mesh dimensions".to_owned());
    }
    let mut uvs = Vec::with_capacity(vertex_n);
    let mut colors = Vec::with_capacity(vertex_n);
    for _ in 0..vertex_n {
        uvs.push([reader.f32()?, reader.f32()?]);
        colors.push([reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?]);
    }
    let mut materials = Vec::with_capacity(material_n);
    for _ in 0..material_n {
        let alpha = reader.u32()?;
        let two_sided = reader.u32()? != 0;
        let alpha_cutoff = reader.f32()?;
        let texture_srgb = reader.u32()? != 0;
        if alpha == 1 && (!alpha_cutoff.is_finite() || !(0.0..=1.0).contains(&alpha_cutoff)) {
            return Err("invalid FPV alpha cutoff".to_owned());
        }
        let width = reader.u32()?;
        let height = reader.u32()?;
        let texture_len = reader.u32()? as usize;
        let texture = if width == 0 && height == 0 && texture_len == 0 {
            None
        } else {
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or("texture dimensions overflow")?;
            if width > 8192 || height > 8192 || expected != texture_len {
                return Err("invalid FPV texture dimensions".to_owned());
            }
            Some((width, height, reader.take(texture_len)?.to_vec()))
        };
        let index_n = reader.u32()? as usize;
        if index_n > 20_000_000 || index_n % 3 != 0 {
            return Err("invalid FPV triangle index count".to_owned());
        }
        let mut indices = Vec::with_capacity(index_n);
        for _ in 0..index_n {
            let index = reader.u32()?;
            if index as usize >= vertex_n {
                return Err("FPV index is outside the vertex buffer".to_owned());
            }
            indices.push(index);
        }
        materials.push(ModelMaterialWire {
            alpha,
            two_sided,
            alpha_cutoff,
            texture_srgb,
            texture,
            indices,
        });
    }
    if !reader.finished() {
        return Err("unexpected bytes after FPV model".to_owned());
    }
    Ok(ModelWire {
        fingerprint,
        uvs,
        colors,
        materials,
    })
}

struct PoseWire {
    fingerprint: u64,
    visible: bool,
    transform: Mat4,
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
}

#[derive(Default)]
struct PoseBlend {
    previous: Option<PoseWire>,
    current: Option<PoseWire>,
    stamp: Option<std::time::SystemTime>,
    received_at: f32,
    interval: f32,
}

impl PoseBlend {
    fn receive(&mut self, pose: PoseWire, stamp: Option<std::time::SystemTime>, now: f32) {
        if stamp.is_some() && stamp == self.stamp {
            return;
        }
        let compatible = self.current.as_ref().is_some_and(|p| {
            p.fingerprint == pose.fingerprint
                && p.positions.len() == pose.positions.len()
                && p.visible
                && pose.visible
        });
        self.previous = if compatible {
            self.current.take()
        } else {
            None
        };
        let dt = now - self.received_at;
        self.interval = if compatible && dt > 0.0 && dt < 0.1 {
            dt.clamp(1.0 / 240.0, 1.0 / 30.0)
        } else {
            0.0
        };
        self.current = Some(pose);
        self.received_at = now;
        self.stamp = stamp;
    }

    fn sample(&self, now: f32) -> Option<PoseWire> {
        let current = self.current.as_ref()?;
        let previous = self
            .previous
            .as_ref()
            .filter(|_| self.interval > 0.0 && current.visible);
        let alpha = if self.interval > 0.0 {
            ((now - self.received_at) / self.interval).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mix =
            |a: &[f32; 3], b: &[f32; 3]| Vec3::from_array(*a).lerp(Vec3::from_array(*b), alpha);
        let positions = match previous {
            Some(p) => p
                .positions
                .iter()
                .zip(&current.positions)
                .map(|(a, b)| mix(a, b).to_array())
                .collect(),
            None => current.positions.clone(),
        };
        let normals = match previous {
            Some(p) => p
                .normals
                .iter()
                .zip(&current.normals)
                .map(|(a, b)| mix(a, b).normalize_or_zero().to_array())
                .collect(),
            None => current.normals.clone(),
        };
        let transform = match previous {
            Some(p) => {
                let a = Transform::from_matrix(p.transform);
                let b = Transform::from_matrix(current.transform);
                Transform {
                    translation: a.translation.lerp(b.translation, alpha),
                    rotation: a.rotation.slerp(b.rotation, alpha),
                    scale: a.scale.lerp(b.scale, alpha),
                }
                .to_matrix()
            }
            None => current.transform,
        };
        Some(PoseWire {
            fingerprint: current.fingerprint,
            visible: current.visible,
            transform,
            positions,
            normals,
        })
    }
}

fn parse_pose(payload: &[u8]) -> Result<PoseWire, String> {
    let mut reader = WireReader {
        bytes: payload,
        at: 0,
    };
    let fingerprint = reader.u64()?;
    let visible = reader.u32()? != 0;
    let vertex_n = reader.u32()? as usize;
    if vertex_n > 1_000_000 || (visible && vertex_n == 0) {
        return Err("invalid FPV pose vertex count".to_owned());
    }
    let mut matrix = [0.0; 16];
    for value in &mut matrix {
        *value = reader.f32()?;
    }
    let mut positions = Vec::with_capacity(vertex_n);
    let mut normals = Vec::with_capacity(vertex_n);
    for _ in 0..vertex_n {
        positions.push([reader.f32()?, reader.f32()?, reader.f32()?]);
        normals.push([reader.f32()?, reader.f32()?, reader.f32()?]);
    }
    if !reader.finished() {
        return Err("unexpected bytes after FPV pose".to_owned());
    }
    Ok(PoseWire {
        fingerprint,
        visible,
        transform: Mat4::from_cols_array(&matrix),
        positions,
        normals,
    })
}

fn material_blend(alpha: u32) -> (benilla_formats::ModelBlend, bool) {
    match alpha {
        1 => (benilla_formats::ModelBlend::AlphaTest, false),
        2 => (benilla_formats::ModelBlend::Blend, false),
        3 => (benilla_formats::ModelBlend::Blend, true),
        4 => (benilla_formats::ModelBlend::Mod, false),
        _ => (benilla_formats::ModelBlend::Opaque, false),
    }
}

fn install_model(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    batch: &mut benilla_world::model_render::M2BatchMaterials,
    camera: Entity,
    model: &ModelWire,
) -> Result<
    (
        Vec<Entity>,
        Vec<Handle<Mesh>>,
        Vec<Handle<benilla_assets::materials::WowModelMaterial>>,
    ),
    String,
> {
    if !batch.ready() {
        return Err("Warcraft shared light buffer is not ready yet".to_owned());
    }
    let mut entities = Vec::new();
    let mut mesh_handles = Vec::new();
    let mut material_handles = Vec::new();
    for (order, source) in model.materials.iter().enumerate() {
        if source.indices.is_empty() {
            continue;
        }
        let texture = source.texture.as_ref().map(|(width, height, rgba)| {
            images.add(Image::new_fill(
                bevy::render::render_resource::Extent3d {
                    width: *width,
                    height: *height,
                    depth_or_array_layers: 1,
                },
                bevy::render::render_resource::TextureDimension::D2,
                rgba,
                if source.texture_srgb {
                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb
                } else {
                    bevy::render::render_resource::TextureFormat::Rgba8Unorm
                },
                bevy::asset::RenderAssetUsages::default(),
            ))
        });
        let (blend, additive) = material_blend(source.alpha);
        let material = batch
            .passthrough(
                blend,
                // Preserve both sides of the streamed first-person mesh. Some MW2 viewmodel
                // pieces expose their back faces as the ADS camera shifts; Warcraft's usual
                // M2 back-face culling makes those pieces disappear from that angle.
                true,
                additive,
                texture,
                order as u16,
                source.alpha_cutoff,
            )
            .ok_or_else(|| "Warcraft shared light buffer is not ready yet".to_owned())?;
        // The normal M2 builder parks the material until its first visible frame.
        // These streamed meshes must configure their light buffer and FPV flags
        // BEFORE that frame; get_mut on a parked handle silently returns None.
        if !benilla_world::model_render::lazy::realize(batch.materials(), material.id()) {
            return Err("Streamed material could not be realized".to_owned());
        }
        if blend == benilla_formats::ModelBlend::AlphaTest {
            if let Some(asset) = batch.materials().get_mut(&material) {
                asset.base.alpha_mode = AlphaMode::Mask(source.alpha_cutoff.clamp(0.0, 1.0));
            }
        }
        material_handles.push(material.clone());
        if std::env::var("CODCRAFT_REALISTIC_LIGHTING").as_deref() == Ok("1")
            && !additive
            && matches!(
                blend,
                benilla_formats::ModelBlend::Opaque
                    | benilla_formats::ModelBlend::AlphaTest
                    | benilla_formats::ModelBlend::Blend
            )
        {
            if let Some(asset) = batch.materials().get_mut(&material) {
                asset.extension.clutter_fade.z =
                    ((asset.extension.clutter_fade.z as u32) | 0x4000) as f32;
                // The bridge has no authored roughness/metalness yet: use a dielectric baseline
                // so skin and painted gun parts are not incorrectly turned into bare metal.
                asset.extension.sidn =
                    Vec4::new(0.48, 0.0, if source.texture_srgb { 1.0 } else { 0.0 }, 0.0);
                info!(
                    "CoDCraft: modern lighting attached to FPV material {}",
                    order
                );
            }
        }
        let mut mesh = Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0; 3]; model.uvs.len()]);
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0.0, 1.0, 0.0]; model.uvs.len()],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, model.uvs.clone());
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, model.colors.clone());
        mesh.insert_indices(bevy::mesh::Indices::U32(source.indices.clone()));
        let mesh = meshes.add(mesh);
        let entity = commands
            .spawn((
                Name::new("CoDCraft live FPV mesh"),
                CodcraftViewmodel,
                bevy::mesh::Mesh3d(mesh.clone()),
                bevy::pbr::MeshMaterial3d(material),
                Transform::default(),
                Visibility::Hidden,
                bevy::camera::visibility::NoFrustumCulling,
                ChildOf(camera),
            ))
            .id();
        entities.push(entity);
        mesh_handles.push(mesh);
    }
    if entities.is_empty() {
        return Err("FPV model contains no drawable material groups".to_owned());
    }
    Ok((entities, mesh_handles, material_handles))
}

fn hide_viewmodel(
    stage: &ViewmodelStage,
    query: &mut Query<(&mut Transform, &mut Visibility), With<CodcraftViewmodel>>,
) {
    for entity in &stage.entities {
        if let Ok((_, mut visibility)) = query.get_mut(*entity) {
            *visibility = Visibility::Hidden;
        }
    }
}

/// Read the source's live pose and draw its actual triangles through Benilla's world camera and
/// shared Warcraft lighting. The Warcraft GUI remains on its normal UI cameras above the scene.
#[derive(Default)]
struct ViewmodelReader {
    mailbox: Option<std::sync::Arc<std::sync::Mutex<Option<ViewmodelFrame>>>>,
    model: Option<std::sync::Arc<ModelWire>>,
    received: Option<std::time::Instant>,
}

struct ViewmodelFrame {
    pose: PoseWire,
    stamp: Option<SystemTime>,
    received: std::time::Instant,
    model: std::sync::Arc<ModelWire>,
}

impl ViewmodelReader {
    fn take(&mut self, paths: &ViewmodelPaths) -> Option<ViewmodelFrame> {
        if self.mailbox.is_none() {
            let mailbox = std::sync::Arc::new(std::sync::Mutex::new(None));
            let weak = std::sync::Arc::downgrade(&mailbox);
            let pose_path = paths.pose.clone();
            let model_path = paths.model.clone();
            std::thread::spawn(move || {
                let mut last_stamp = None;
                let mut model: Option<std::sync::Arc<ModelWire>> = None;
                loop {
                    if weak.strong_count() == 0 { break; }
                    let stamp = std::fs::metadata(&pose_path).ok().and_then(|m| m.modified().ok());
                    if stamp.is_some() && stamp != last_stamp {
                        if let Ok(pose) = read_packet(&pose_path, POSE_MAGIC).and_then(|p| parse_pose(&p)) {
                            if model.as_ref().is_none_or(|m| m.fingerprint != pose.fingerprint) {
                                if let Ok(next) = read_packet(&model_path, MODEL_MAGIC).and_then(|p| parse_model(&p)) {
                                    if next.fingerprint == pose.fingerprint { model = Some(std::sync::Arc::new(next)); }
                                }
                            }
                            if let Some(model) = model.as_ref().filter(|m| m.fingerprint == pose.fingerprint) {
                                let frame = ViewmodelFrame { pose, stamp, received: std::time::Instant::now(), model: model.clone() };
                                if let Some(mailbox) = weak.upgrade() {
                                    if let Ok(mut slot) = mailbox.lock() { *slot = Some(frame); }
                                }
                                last_stamp = stamp;
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(4));
                }
            });
            self.mailbox = Some(mailbox);
        }
        let frame = self.mailbox.as_ref()?.try_lock().ok()?.take()?;
        self.received = Some(frame.received);
        self.model = Some(frame.model.clone());
        Some(frame)
    }
}

fn present_viewmodel(
    mut commands: Commands,
    world_live: Res<benilla_world::schedule::WorldLive>,
    keys: Res<ButtonInput<KeyCode>>,
    capture: Res<crate::ui_script::UiKeyboardCapture>,
    time: Res<Time>,
    link: Res<GuestLink>,
    paths: Option<Res<ViewmodelPaths>>,
    mut view: ResMut<GuestView>,
    mut stage: ResMut<ViewmodelStage>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut batch: benilla_world::model_render::M2BatchMaterials,
    cameras: Query<Entity, With<benilla_world::view::WorldCamera>>,
    mut viewmodel_entities: Query<(&mut Transform, &mut Visibility), With<CodcraftViewmodel>>,
    stream: (Local<PoseBlend>, Local<ViewmodelReader>),
    gear: (Res<gear::GearState>, Res<predator::Predator>),
) {
    let (gear, predator) = gear;
    let _work_scope = profile::scope("codcraft.rs:present_viewmodel");
    let (mut smoothing, mut reader) = stream;
    // The equipped weapon is gameplay, not a Q-key diagnostic overlay toggle.
    view.shown = true;
    let Some(paths) = paths else { return };
    if predator.flying() || !world_live.0 || (gear::enabled() && gear.code == 0) {
        *smoothing = PoseBlend::default();
        hide_viewmodel(&stage, &mut viewmodel_entities);
        return;
    }
    let guest_live = link.state().error.is_none()
        && time.elapsed_secs() - link.state().read_at <= 0.5
        && link
            .state()
            .players
            .iter()
            .any(|player| player.health > 0 && player.max_health > 0);
    if let Some(frame) = reader.take(&paths) {
        smoothing.receive(frame.pose, frame.stamp, time.elapsed_secs());
    }
    let pose_fresh = reader.received.is_some_and(|received| received.elapsed() <= std::time::Duration::from_secs(2));
    if !guest_live || !pose_fresh {
        *smoothing = PoseBlend::default();
        hide_viewmodel(&stage, &mut viewmodel_entities);
        return;
    }

    // Animate between complete native poses at the host frame rate, including write-race frames.
    let Some(pose) = smoothing.sample(time.elapsed_secs()) else {
        return;
    };
    if !pose.visible || !view.shown {
        hide_viewmodel(&stage, &mut viewmodel_entities);
        return;
    }
    if stage.fingerprint != Some(pose.fingerprint) {
        let Some(model) = reader.model.as_ref().filter(|m| m.fingerprint == pose.fingerprint) else { return; };
        let Ok(camera) = cameras.single() else {
            return;
        };
        let (entities, mesh_handles, material_handles) = match install_model(
            &mut commands,
            &mut images,
            &mut meshes,
            &mut batch,
            camera,
            &model,
        ) {
            Ok(installed) => installed,
            Err(error) if error == "Warcraft shared light buffer is not ready yet" => return,
            Err(error) => {
                if view.error.as_deref() != Some(error.as_str()) {
                    warn!("CoDCraft: FPV model install: {error}");
                    view.error = Some(error);
                }
                return;
            }
        };
        for entity in stage.entities.drain(..) {
            commands.entity(entity).despawn();
        }
        for (_, entities) in stage.bot_entities.drain() {
            for entity in entities {
                commands.entity(entity).despawn();
            }
        }
        // Only the camera-attached first-person weapon bypasses distance fog;
        // world rifles and thrown grenades retain their scene fog policy.
        for handle in &material_handles {
            if let Some(asset) = batch.materials().get_mut(handle) {
                asset.extension.clutter_fade.z = benilla_world::model_render::replace_fog_policy(
                    asset.extension.clutter_fade.z,
                    benilla_formats::FogPolicy::Off,
                );
                asset.extension.clutter_fade.z =
                    ((asset.extension.clutter_fade.z as u32) | 0x8000) as f32;
            }
        }
        stage.entities = entities;
        stage.meshes = mesh_handles;
        stage.materials = material_handles;
        stage.fingerprint = Some(model.fingerprint);
        stage.vertex_n = model.uvs.len();
        info!(
            "CoDCraft: 3D FPV model installed ({} material meshes)",
            stage.meshes.len()
        );
    }
    if pose.fingerprint != stage.fingerprint.unwrap_or_default()
        || pose.positions.len() != stage.vertex_n
        || pose.positions.len() != pose.normals.len()
    {
        hide_viewmodel(&stage, &mut viewmodel_entities);
        return;
    }
    for handle in &stage.meshes {
        if let Some(mesh) = meshes.get_mut(handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pose.positions.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, pose.normals.clone());
        }
    }
    for entity in &stage.entities {
        if let Ok((mut transform, mut visibility)) = viewmodel_entities.get_mut(*entity) {
            *transform = Transform::from_matrix(pose.transform);
            *visibility = Visibility::Visible;
        }
    }
    stage.pose_transform = Some(pose.transform);
    if view.error.take().is_some() {
        info!("CoDCraft: live FPV model stream recovered");
    }
}

fn live_kobold(
    _guid: &crate::net::Guid,
    entity: &crate::net::NetEntity,
    store: &crate::net::ObjectStore,
) -> bool {
    entity.kind == benilla_protocol::EntityKind::Unit
        && store.0.unit_max_health().unwrap_or(0) > 0
        && store.0.unit_health().unwrap_or(0) > 0
}

/// Supply Warcraft observations to IW4 and forward only fresh native controller commands.
fn drive_kobold_ai(
    time: Res<Time>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    link: Res<GuestLink>,
    input: Res<GuestInputPublisher>,
    player: Res<crate::player::Player>,
    rifle_aims: Res<kobold_pose::RifleAims>,
    self_guid: Res<crate::net::SelfGuid>,
    mut ai: ResMut<CodcraftKoboldAi>,
    net: Option<Res<crate::net::NetCommands>>,
    collision: benilla_world::collision::WorldCollision,
    kobolds: Query<
        (
            &crate::net::Guid,
            &crate::net::NetEntity,
            &crate::net::ObjectStore,
            &Transform,
        ),
        (
            With<RifleEnemy>,
            Without<crate::net::SelfPlayer>,
            Without<crate::net::Embodied>,
        ),
    >,
) {
    let _work_scope = profile::scope("codcraft.rs:drive_kobold_ai");
    let now = time.elapsed_secs();
    let ready = world_live.0
        && input.owns_gameplay_controls()
        && player.active
        && link.state().error.is_none()
        && now - link.state().read_at <= 0.5;
    let Some(target_guid) = self_guid.0 else {
        return;
    };
    let Some(net) = net else {
        return;
    };
    if !ready {
        return;
    }
    if now < ai.next_poll {
        return;
    }
    ai.next_poll = now + 0.10;
    let Some(raw) = std::env::var_os("CODCRAFT_STATE") else {
        return;
    };
    let base = std::path::PathBuf::from(raw);
    let to_iw4 = |p: Vec3| benilla_assets::coords::bevy_to_wow(p).map(|v| v * 36.0);
    // Service native controller hull requests against the host's real terrain/WMO bodies.
    // Queries are denied on the guest until this response arrives; no guest-map fallback.
    if let Ok(queries) = std::fs::read(base.with_extension("botqueries")) {
        if queries.len() >= 12 && &queries[..4] == b"CCTQ" && le32(&queries, 4) == 1 {
            let count = le32(&queries, 8) as usize;
            if count <= 256 && queries.len() == 12 + count * 24 {
                let mut results = Vec::new();
                results.extend_from_slice(b"CCTR");
                results.extend_from_slice(&1u32.to_le_bytes());
                results.extend_from_slice(&(count as u32).to_le_bytes());
                let shape =
                    avian3d::prelude::Collider::cuboid(30.0 / 36.0, 70.0 / 36.0, 30.0 / 36.0);
                let mut valid = true;
                for i in 0..count {
                    let at = 12 + i * 24;
                    let values: [f32; 6] =
                        std::array::from_fn(|j| f32::from_bits(le32(&queries, at + j * 4)));
                    if !values.iter().all(|v| v.is_finite()) {
                        valid = false;
                        break;
                    }
                    let from = benilla_assets::coords::wow_to_bevy([
                        values[0] / 36.0,
                        values[1] / 36.0,
                        values[2] / 36.0,
                    ]);
                    let end = benilla_assets::coords::wow_to_bevy([
                        values[3] / 36.0,
                        values[4] / 36.0,
                        values[5] / 36.0,
                    ]);
                    let movement = end - from;
                    if from.distance(player.pos) > 80.0 || movement.length() > 100.0 {
                        valid = false;
                        break;
                    }
                    let hit =
                        collision.cast_body(&shape, from + Vec3::Y * (35.0 / 36.0), movement, 0.01);
                    let fraction = hit
                        .as_ref()
                        .map(|h| (h.distance / movement.length().max(0.0001)).clamp(0.0, 1.0))
                        .unwrap_or(1.0);
                    let normal = hit
                        .as_ref()
                        .map(|h| benilla_assets::coords::bevy_to_wow(h.normal1))
                        .unwrap_or([0.0; 3]);
                    let endpos = to_iw4(from + movement * fraction);
                    for value in values {
                        results.extend_from_slice(&((value / 2.0).round() as i32).to_le_bytes());
                    }
                    results.extend_from_slice(&fraction.to_le_bytes());
                    for value in normal.into_iter().chain(endpos) {
                        results.extend_from_slice(&value.to_le_bytes());
                    }
                    results.extend_from_slice(
                        &u32::from(hit.is_some_and(|h| h.intersects())).to_le_bytes(),
                    );
                }
                if valid {
                    let _ = std::fs::write(base.with_extension("bottraces"), results);
                }
            }
        }
    }
    let target = player.pos + Vec3::Y * (48.0 / 36.0);
    ai.clear_shooters.clear();
    let mut records = Vec::new();
    let mut live = Vec::new();
    let mut cover_candidates = Vec::new();
    for (guid, entity, store, transform) in &kobolds {
        if !live_kobold(guid, entity, store) {
            continue;
        }
        let distance = (target - (transform.translation + Vec3::Y * 1.1)).length();
        if distance > KOBOLD_MAX_RANGE || live.len() >= 64 {
            continue;
        }
        let eye = transform.translation + Vec3::Y * (40.0 / 36.0);
        let delta = target - eye;
        let visible = Dir3::new(delta)
            .ok()
            .is_some_and(|dir| collision.ray_los(eye, dir, delta.length()).is_none());
        if !ai.engaged.contains_key(&guid.0) && (!visible || distance > 22.0) {
            continue;
        }
        if visible {
            ai.engaged.insert(guid.0, now);
            ai.clear_shooters.push(guid.0);
            cover_candidates.push((guid.0, transform.translation));
        } else if ai.engaged.get(&guid.0).is_none_or(|seen| now - *seen > 5.0) {
            continue;
        }
        live.push(guid.0);
        records.extend_from_slice(&guid.0.to_le_bytes());
        for value in to_iw4(transform.translation) {
            records.extend_from_slice(&value.to_le_bytes());
        }
        // IW4 tactics interpret health on a 0..100 scale; raw level-1 WoW HP would
        // incorrectly mark a full-health worker as wounded and retreating.
        let health = ((u64::from(store.0.unit_health().unwrap_or(0)) * 100)
            / u64::from(store.0.unit_max_health().unwrap_or(1).max(1)))
        .min(100) as u32;
        records.extend_from_slice(&health.to_le_bytes());
        records.extend_from_slice(&u32::from(visible).to_le_bytes());
    }
    // Bound cover work: one actor every 150 ms, genuine ground/body/LOS checks.
    if now >= ai.next_cover_scan && !cover_candidates.is_empty() {
        ai.next_cover_scan = now + 0.15;
        let (guid, origin) = cover_candidates[ai.cover_scan % cover_candidates.len()];
        ai.cover_scan = ai.cover_scan.wrapping_add(1);
        let shape = avian3d::prelude::Collider::cuboid(0.65, 1.65, 0.65);
        let mut found = None;
        for i in 0..16 {
            let angle = (i % 8) as f32 * std::f32::consts::TAU / 8.0;
            let candidate =
                origin + Vec3::new(angle.cos(), 0.0, angle.sin()) * if i < 8 { 3.0 } else { 5.0 };
            let Some(ground) = collision.ray_body(candidate + Vec3::Y * 3.0, Dir3::NEG_Y, 6.0)
            else {
                continue;
            };
            let hide = candidate + Vec3::Y * (3.0 - ground.distance);
            if (hide.y - origin.y).abs() > 1.0 {
                continue;
            }
            if collision
                .cast_body(&shape, origin + Vec3::Y * 1.2, hide - origin, 0.01)
                .is_some()
            {
                continue;
            }
            let ray = target - (hide + Vec3::Y * 1.1);
            if Dir3::new(ray).ok().is_some_and(|d| {
                collision
                    .ray_los(hide + Vec3::Y * 1.1, d, ray.length())
                    .is_some()
            }) {
                found = Some((hide, origin, now));
                break;
            }
        }
        if let Some(cover) = found {
            ai.cover.insert(guid, cover);
        }
    }
    ai.cover
        .retain(|g, (_, _, at)| live.contains(g) && now - *at < 6.0);
    let mut cover_bytes = Vec::new();
    cover_bytes.extend_from_slice(b"CCCV");
    cover_bytes.extend_from_slice(&1u32.to_le_bytes());
    cover_bytes.extend_from_slice(&(ai.cover.len() as u32).to_le_bytes());
    for (guid, (hide, peek, _)) in &ai.cover {
        cover_bytes.extend_from_slice(&guid.to_le_bytes());
        for value in to_iw4(*hide).into_iter().chain(to_iw4(*peek)) {
            cover_bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let _ = std::fs::write(base.with_extension("botcover"), cover_bytes);
    ai.engaged.retain(|g, _| live.contains(g));
    ai.sequence = ai.sequence.wrapping_add(1);
    let mut observations = Vec::new();
    observations.extend_from_slice(b"CCBO");
    observations.extend_from_slice(&1u32.to_le_bytes());
    observations.extend_from_slice(&ai.sequence.to_le_bytes());
    observations.extend_from_slice(&(live.len() as u32).to_le_bytes());
    for value in to_iw4(player.pos) {
        observations.extend_from_slice(&value.to_le_bytes());
    }
    observations.extend_from_slice(
        &link
            .state()
            .players
            .first()
            .map(|p| p.weapon)
            .unwrap_or(0)
            .to_le_bytes(),
    );
    observations.extend_from_slice(&((now * 1000.0) as u32).to_le_bytes());
    observations.extend_from_slice(&records);
    if std::fs::write(base.with_extension("botobs"), observations).is_err() {
        return;
    }
    let output = base.with_extension("botcmd");
    let fresh = std::fs::metadata(&output)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs_f32() < 0.5);
    if !fresh {
        return;
    }
    let Ok(bytes) = std::fs::read(output) else {
        return;
    };
    if bytes.len() < 16 || &bytes[..4] != b"CCBC" || le32(&bytes, 4) != 1 {
        return;
    }
    let seq = le32(&bytes, 8);
    let count = le32(&bytes, 12) as usize;
    if seq == ai.consumed || count > 64 || bytes.len() != 16 + count * 24 {
        return;
    }
    ai.consumed = seq;
    let allowed = ai.command_budget.take(now, count);
    let start = ai.command_cursor % count.max(1);
    ai.command_cursor = ai.command_cursor.wrapping_add(allowed);
    for offset in 0..allowed {
        let i = (start + offset) % count;
        let at = 16 + i * 24;
        let guid = le64(&bytes, at);
        let yaw = f32::from_bits(le32(&bytes, at + 8));
        if !live.contains(&guid) || !yaw.is_finite() {
            continue;
        }
        let shot_sequence = le32(&bytes, at + 20);
        let Some((_, _, _, transform)) = kobolds.iter().find(|(g, _, _, _)| g.0 == guid) else {
            continue;
        };
        let aim = rifle_aims.0.get(&guid).copied();
        let muzzle = aim
            .map(|a| a.muzzle)
            .unwrap_or(transform.translation + Vec3::Y * 1.1);
        let target = player.pos + Vec3::Y * 1.2;
        let can_fire = aim.is_some_and(|a| a.aligned(target, now))
            && ai.clear_shooters.contains(&guid)
            && Dir3::new(target - muzzle).ok().is_some_and(|dir| {
                collision
                    .ray_los(muzzle, dir, muzzle.distance(target))
                    .is_none()
            });
        let (hit, endpoint) = shot_math::shot(
            guid,
            shot_sequence,
            transform.translation.distance(player.pos),
            muzzle,
            target,
        );
        let shot = ai.shots.entry(guid).or_insert((0, -1.0));
        let fired = shot_sequence != 0 && shot_sequence != shot.0 && can_fire;
        if fired {
            *shot = (shot_sequence, now);
            ai.pending_shots.push_back((guid, now, endpoint));
            if ai.pending_shots.len() > 128 {
                ai.pending_shots.pop_front();
            }
        }
        let mut forward = le32(&bytes, at + 12) as i32;
        let mut right = le32(&bytes, at + 16) as i32;
        let (s, c) = yaw.to_radians().sin_cos();
        let movement = benilla_assets::coords::wow_to_bevy([
            (c * forward as f32 + s * right as f32) / 127.0 * 3.0,
            (s * forward as f32 - c * right as f32) / 127.0 * 3.0,
            0.0,
        ]);
        let shape = avian3d::prelude::Collider::cuboid(0.65, 1.65, 0.65);
        if movement.length_squared() > 0.0
            && collision
                .cast_body(
                    &shape,
                    transform.translation + Vec3::Y * 1.2,
                    movement,
                    0.01,
                )
                .is_some()
        {
            forward = 0;
            right = 0;
            // Try a clear lateral route instead of remaining pinned against an obstacle.
            let side = if guid & 1 == 0 { 110 } else { -110 };
            for candidate in [side, -side] {
                let lateral = benilla_assets::coords::wow_to_bevy([
                    s * candidate as f32 / 127.0 * 3.0,
                    -c * candidate as f32 / 127.0 * 3.0,
                    0.0,
                ]);
                if collision
                    .cast_body(&shape, transform.translation + Vec3::Y * 1.2, lateral, 0.01)
                    .is_none()
                {
                    right = candidate;
                    break;
                }
            }
        }
        let _ = net.0.send(crate::net::ClientCommand::CodcraftNpcBullet {
            attacker_guid: guid,
            target_guid,
            yaw: yaw.to_radians(),
            forward,
            right,
            shot_sequence: if !fired {
                0
            } else if hit {
                shot_sequence
            } else {
                shot_sequence | 0x80000000
            },
        });
    }
    ai.shots.retain(|guid, _| live.contains(guid));
}

/// Attach weapon-only native grip geometry to the rig's animated right-hand attachment.
fn update_kobold_weapons(
    mut commands: Commands,
    time: Res<Time>,
    ai: Res<CodcraftKoboldAi>,
    world_live: Res<benilla_world::schedule::WorldLive>,
    link: Res<GuestLink>,
    input: Res<GuestInputPublisher>,
    player: Res<crate::player::Player>,
    mut stage: ResMut<ViewmodelStage>,
    gun: Res<KoboldGunStage>,
    wow_materials: Res<Assets<benilla_assets::materials::WowModelMaterial>>,
    mut diagnostic_materials: ResMut<Assets<StandardMaterial>>,
    mut diagnostic_cache: Local<
        std::collections::HashMap<
            bevy::asset::AssetId<benilla_assets::materials::WowModelMaterial>,
            Handle<StandardMaterial>,
        >,
    >,
    mut kobolds: Query<
        (
            Entity,
            &crate::net::Guid,
            &crate::net::NetEntity,
            &crate::net::ObjectStore,
            &crate::entities::BoneAttach,
            Option<&crate::entities::HeldAttached>,
            &mut benilla_world::rig_anim::RigPose,
        ),
        (
            Without<crate::net::SelfPlayer>,
            Without<crate::net::Embodied>,
            Without<CodcraftKoboldWeapon>,
        ),
    >,
    mut weapons: Query<(&mut Transform, &mut Visibility), With<CodcraftKoboldWeapon>>,
    frames: Query<&GlobalTransform>,
    cameras: Query<Entity, With<benilla_world::view::WorldCamera>>,
) {
    let _work_scope = profile::scope("codcraft.rs:update_kobold_weapons");
    let probe_mode = std::env::var_os("CODCRAFT_WEAPON_DRAW_PROBE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let detached = probe_mode.trim() == "detached";
    let ready = world_live.0
        && input.owns_gameplay_controls()
        && player.active
        && link.state().error.is_none()
        && stage.fingerprint.is_some()
        && gun.fingerprint.is_some()
        && !gun.meshes.is_empty()
        && gun.meshes.len() == gun.materials.len();
    if !ready {
        for entities in stage.bot_entities.values() {
            for entity in entities {
                if let Ok((_, mut visibility)) = weapons.get_mut(*entity) {
                    *visibility = Visibility::Hidden;
                }
            }
        }
        return;
    }

    let mut active = Vec::new();
    for (root, guid, entity, store, attach, held, mut rig) in &mut kobolds {
        if !live_kobold(guid, entity, store) {
            continue;
        }
        let Some(&(bone, offset)) = attach.points.get(&crate::entities::attach_id::HAND_RIGHT)
        else {
            continue;
        };
        let Some(hand) = rig.anchor_for(&mut commands, root, bone) else {
            continue;
        };
        let age = ai
            .shots
            .get(&guid.0)
            .map(|shot| time.elapsed_secs() - shot.1)
            .unwrap_or(1.0);
        let kick = (1.0 - age / 0.12).clamp(0.0, 1.0);
        let hand_frame = kobold_pose::bone_frame(&rig, bone as usize);
        let root_frame = frames.get(rig.joints_root).copied().unwrap_or_default();
        let grip = hand_frame.transform_point3(offset);
        let target = root_frame
            .affine()
            .inverse()
            .transform_point3(player.pos + Vec3::Y * 1.2);
        let forward = (target - grip).normalize_or(Vec3::NEG_Z);
        let (_, hand_rotation, _) = hand_frame.to_scale_rotation_translation();
        let local = Transform {
            translation: offset - hand_rotation.inverse() * forward * (0.025 * kick),
            rotation: combat_math::mount_rotation(gun.forward, gun.up, forward, hand_rotation),
            ..Default::default()
        };
        // Replacement is visual-only: retain the descriptor/server weapon for damage rolls.
        if let Some(held) = held {
            for old in held.spawned_slots().iter().take(3).flatten() {
                commands.entity(*old).insert(Visibility::Hidden);
            }
        }
        active.push(guid.0);
        let entries = if let Some(entries) = stage.bot_entities.get(&guid.0) {
            entries.clone()
        } else {
            let mut spawned = Vec::with_capacity(gun.meshes.len());
            for (mesh, material) in gun.meshes.iter().zip(&gun.materials) {
                let mut spawned_entity = commands.spawn((
                    Name::new("CoDCraft Kobold weapon"),
                    CodcraftKoboldWeapon,
                    bevy::mesh::Mesh3d(mesh.clone()),
                    bevy::pbr::MeshMaterial3d(material.clone()),
                    local,
                    Visibility::Visible,
                    bevy::camera::visibility::NoFrustumCulling,
                    ChildOf(hand),
                ));
                // Opt-in material isolation: identical retail mesh/texture and
                // hand transform, without the host's custom M2 fragment path.
                if std::env::var_os("CODCRAFT_WEAPON_MATERIAL_TEST").is_some() {
                    if let Some(source) = wow_materials.get(material) {
                        let debug = diagnostic_cache
                            .entry(material.id())
                            .or_insert_with(|| {
                                diagnostic_materials.add(StandardMaterial {
                                    base_color: if std::env::var_os("CODCRAFT_WEAPON_FLAT_TEST")
                                        .is_some()
                                    {
                                        Color::srgb(1.0, 0.0, 1.0)
                                    } else {
                                        Color::WHITE
                                    },
                                    base_color_texture: if std::env::var_os(
                                        "CODCRAFT_WEAPON_FLAT_TEST",
                                    )
                                    .is_some()
                                    {
                                        None
                                    } else {
                                        source.base.base_color_texture.clone()
                                    },
                                    unlit: true,
                                    cull_mode: None,
                                    ..Default::default()
                                })
                            })
                            .clone();
                        spawned_entity.remove::<bevy::pbr::MeshMaterial3d<benilla_assets::materials::WowModelMaterial>>()
                              .insert(bevy::pbr::MeshMaterial3d(debug));
                    }
                }
                let entity = spawned_entity.id();
                spawned.push(entity);
            }
            stage.bot_entities.insert(guid.0, spawned.clone());
            info!(
                "CoDCraft: armed Kobold {:#x} at right-hand bone {}, {} gun parts",
                guid.0,
                bone,
                spawned.len()
            );
            spawned
        };
        for entity in entries {
            let parent = if detached {
                cameras.single().ok().unwrap_or(hand)
            } else {
                hand
            };
            commands.entity(entity).insert(ChildOf(parent));
            if let Ok((mut weapon_transform, mut visibility)) = weapons.get_mut(entity) {
                *weapon_transform = if detached {
                    Transform::from_translation(Vec3::new(0.0, 0.0, -2.0))
                        .with_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2))
                } else {
                    local
                };
                *visibility = Visibility::Visible;
            }
        }
    }

    let stale: Vec<u64> = stage
        .bot_entities
        .keys()
        .copied()
        .filter(|guid| !active.contains(guid))
        .collect();
    for guid in stale {
        if let Some(entities) = stage.bot_entities.remove(&guid) {
            for entity in entities {
                commands.entity(entity).despawn();
            }
        }
    }
}

fn audit_kobold_weapons(
    mut commands: Commands,
    time: Res<Time>,
    mut next: Local<f32>,
    stage: Res<ViewmodelStage>,
    meshes: Res<Assets<Mesh>>,
    weapons: Query<
        (
            &GlobalTransform,
            &InheritedVisibility,
            &ViewVisibility,
            &bevy::mesh::Mesh3d,
        ),
        With<CodcraftKoboldWeapon>,
    >,
    cameras: Query<(&Camera, &GlobalTransform), With<benilla_world::view::WorldCamera>>,
    mut probe: Local<(String, f32, bool)>,
    anchors: Query<(&ChildOf, &Transform)>,
    rigs: Query<(
        &crate::net::Guid,
        &benilla_world::rig_anim::RigPose,
        &crate::entities::BoneAttach,
    )>,
    frames: Query<&GlobalTransform>,
) {
    let _work_scope = profile::scope("codcraft.rs:audit_kobold_weapons");
    if !stage.bot_entities.is_empty() {
        if let Some(path) = std::env::var_os("CODCRAFT_WEAPON_DRAW_PROBE") {
            let path = PathBuf::from(path);
            if let Ok(mode) = std::fs::read_to_string(&path) {
                let mode = mode.trim().to_owned();
                if probe.0 != mode {
                    *probe = (mode.clone(), time.elapsed_secs() + 1.0, false);
                }
                if !probe.2 && time.elapsed_secs() >= probe.1 {
                    let out = path.with_file_name(format!("rifle-probe-{mode}.png"));
                    commands
                        .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                        .observe(bevy::render::view::screenshot::save_to_disk(out));
                    probe.2 = true;
                }
            }
        }
    }
    if std::env::var_os("CODCRAFT_WEAPON_AUDIT").is_none() || time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    for (camera, frame) in &cameras {
        info!(
            "CoDCraft camera audit active={} position={:?} viewport={:?}",
            camera.is_active,
            frame.translation(),
            camera.logical_viewport_size()
        );
    }
    for (guid, parts) in &stage.bot_entities {
        if let Some((_, rig, attach)) = rigs.iter().find(|(g, _, _)| g.0 == *guid) {
            if let Some(&(bone, offset)) =
                attach.points.get(&crate::entities::attach_id::HAND_RIGHT)
            {
                let root = frames.get(rig.joints_root).ok();
                let native = rig.model.get(bone as usize).copied();
                let naive = kobold_pose::bone_frame(rig, bone as usize);
                info!(
                    "CoDCraft hand audit guid={guid:#x} root={:?} bone={} offset={offset:?} native={native:?} naive={naive:?}",
                    root.map(|g| g.translation()),
                    bone
                );
            }
        }
        for entity in parts {
            let Ok((frame, inherited, viewed, mesh)) = weapons.get(*entity) else {
                continue;
            };
            let Some(mesh) = meshes.get(&mesh.0) else {
                continue;
            };
            let Some(bevy::mesh::VertexAttributeValues::Float32x3(vertices)) =
                mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            else {
                continue;
            };
            let mut low = Vec3::splat(f32::INFINITY);
            let mut high = Vec3::splat(f32::NEG_INFINITY);
            let mut screen_low = Vec2::splat(f32::INFINITY);
            let mut screen_high = Vec2::splat(f32::NEG_INFINITY);
            let mut projection_error = None;
            let indices: Vec<usize> = mesh
                .indices()
                .map(|i| i.iter().collect())
                .unwrap_or_default();
            for i in indices {
                if let Some(vertex) = vertices.get(i) {
                    let p = frame.transform_point(Vec3::from_array(*vertex));
                    low = low.min(p);
                    high = high.max(p);
                    if let Ok((camera, camera_frame)) = cameras.single() {
                        match camera.world_to_viewport(camera_frame, p) {
                            Ok(pixel) => {
                                screen_low = screen_low.min(pixel);
                                screen_high = screen_high.max(pixel);
                            }
                            Err(error) => projection_error = Some(error),
                        }
                    }
                }
            }
            info!(
                "CoDCraft weapon audit guid={guid:#x} entity={entity:?} inherited={} viewed={} origin={:?} bounds={low:?}..{high:?} screen={screen_low:?}..{screen_high:?} projection_error={projection_error:?} parent_local={:?}",
                inherited.get(),
                viewed.get(),
                frame.translation(),
                anchors.get(*entity).ok()
            );
        }
    }
}

/// Open loot only for a creature the CoD bullet path recently damaged. The normal loot pipeline
/// then handles bag-space checks, item storage, money, and server-side loot rights.
fn auto_loot_confirmed_kills(
    time: Res<Time>,
    mut combat: ResMut<CodcraftCombatState>,
    commands: Res<crate::net::NetCommands>,
    mut latch: ResMut<crate::ui_loot::LootLatch>,
    mut loot: ResMut<crate::ui_loot::LootState>,
    units: Query<(
        &crate::net::Guid,
        &crate::net::NetEntity,
        &crate::net::ObjectStore,
    )>,
) {
    let _work_scope = profile::scope("codcraft.rs:auto_loot_confirmed_kills");
    if !passthrough_enabled() {
        return;
    }

    let now = time.elapsed_secs();
    combat
        .confirmed_bullet_hits
        .retain(|_, expires| *expires > now);
    combat.requested_loot.retain(|_, expires| *expires > now);

    // Serialize corpse sessions: replacing the latch loses an outstanding response.
    if let Some(guid) = loot.source() {
        if combat.confirmed_bullet_hits.contains_key(&guid)
            && (loot.is_empty() || !combat.requested_loot.contains_key(&guid))
        {
            crate::ui_loot::close_interaction(&mut loot, &mut latch, Some(&commands));
            // The corpse's lootable flag is authoritative. Retry after a cooldown
            // when storage failed (for example, when bags were full).
            combat
                .requested_loot
                .insert(guid, now + CodcraftCombatState::LOOT_REQUEST_LIFETIME);
        }
        return;
    }
    if let Some(guid) = latch.0 {
        if combat.requested_loot.contains_key(&guid) {
            return;
        }
        if combat.confirmed_bullet_hits.contains_key(&guid) {
            let _ = commands
                .0
                .send(crate::net::ClientCommand::LootRelease { guid });
            latch.0 = None;
            combat
                .requested_loot
                .insert(guid, now + CodcraftCombatState::LOOT_REQUEST_LIFETIME);
            return;
        } else {
            return;
        }
    }

    for (guid, entity, store) in &units {
        if entity.kind != benilla_protocol::EntityKind::Unit
            || !store.0.unit_is_dead()
            || !store.0.unit_lootable()
        {
            continue;
        }

        if combat.take_recent_kill_candidate(guid.0, now) && combat.claim_auto_loot(guid.0, now) {
            if commands
                .0
                .send(crate::net::ClientCommand::Loot { guid: guid.0 })
                .is_ok()
            {
                latch.0 = Some(guid.0);
                info!("CoDCraft: auto-loot requested for {:#x}", guid.0);
            }
            break;
        }
    }
}

/// Registers state/input passthrough and the actual 3D first-person model stream.
impl Plugin for CodcraftPlugin {
    fn build(&self, app: &mut App) {
        ragdoll::plugin(app);
        gear::plugin(app);
        tracers::plugin(app);
        damage_direction::plugin(app);
        grenades::plugin(app);
        effects::plugin(app);
        predator::plugin(app);
        helicopter::plugin(app);
        sentry::plugin(app);
        app.init_resource::<GuestLink>()
            .init_resource::<GuestView>()
            .init_resource::<ViewmodelStage>()
            .init_resource::<KoboldGunStage>()
            .init_resource::<GuestInputPublisher>()
            .init_resource::<CodcraftCombatState>()
            .init_resource::<CodcraftKoboldAi>()
            .init_resource::<CodcraftNearClip>()
            .init_resource::<CodcraftPoseMap>()
            .add_systems(PostUpdate, soldiers::display)
            .add_message::<CodcraftHitMarker>()
            .add_message::<CodcraftBulletImpact>()
            .add_message::<CodcraftDamageText>()
            .add_systems(
                PreStartup,
                (
                    configure,
                    resolve_viewmodel_paths,
                    configure_input,
                    setup_codcraft_hud,
                ),
            )
            .add_systems(
                Update,
                (publish_guest_input, poll, apply_guest_player)
                    .chain()
                    .before(crate::player::PlayerControlSet),
            )
            .add_systems(
                Update,
                (classify_rifle_enemies, drive_kobold_ai)
                    .chain()
                    .after(apply_guest_player),
            )
            .add_systems(PostUpdate, auto_loot_confirmed_kills)
            // The writer runs throughout the app so it can publish a neutral/release packet
            // while the player world is inactive. Actual controls are gated by `WorldLive` above.
            .add_systems(
                Update,
                tune_ads_near_clip
                    .after(publish_guest_input)
                    .before(benilla_world::view::stamp_near_clip),
            )
            // Keep the live mesh path independent of WorldStage's conditional sets; it hides the
            // model until both clients are in-world and the guest pose packet is fresh.
            .add_systems(Update, (present_viewmodel, update_codcraft_hud).chain())
            .add_systems(
                PostUpdate,
                (kobold_pose::face_rifles, kobold_pose::pose_rifles)
                    .chain()
                    .in_set(benilla_world::rig_anim::PosePost),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewmodel_interpolates_between_native_pose_packets() {
        let pose = |x| PoseWire {
            fingerprint: 7,
            visible: true,
            transform: Mat4::IDENTITY,
            positions: vec![[x, 0.0, 0.0]],
            normals: vec![[0.0, 1.0, 0.0]],
        };
        let mut blend = PoseBlend::default();
        let first = std::time::SystemTime::UNIX_EPOCH;
        let second = first + std::time::Duration::from_millis(16);
        blend.receive(pose(0.0), Some(first), 1.0);
        blend.receive(pose(1.0), Some(second), 1.016);
        assert!((blend.sample(1.024).unwrap().positions[0][0] - 0.5).abs() < 0.001);
        // Re-reading the same native frame must not restart the interpolation.
        blend.receive(pose(1.0), Some(second), 1.024);
        assert!((blend.sample(1.032).unwrap().positions[0][0] - 1.0).abs() < 0.001);
        let hidden = PoseWire {
            visible: false,
            ..pose(1.0)
        };
        blend.receive(
            hidden,
            Some(second + std::time::Duration::from_millis(16)),
            1.032,
        );
        assert!(!blend.sample(1.032).unwrap().visible);
    }

    #[test]
    fn auto_loot_retries_without_consuming_confirmed_kill() {
        let mut combat = CodcraftCombatState::default();
        combat.confirm_bullet_hit(7, 0.0);
        assert!(combat.take_recent_kill_candidate(7, 0.0));
        assert!(combat.claim_auto_loot(7, 0.0));
        assert!(!combat.claim_auto_loot(7, 1.0));
        assert!(combat.take_recent_kill_candidate(7, 4.0));
        assert!(combat.claim_auto_loot(7, 4.0));
        assert!(!combat.take_recent_kill_candidate(7, 121.0));
    }

    #[derive(Resource, Default)]
    struct ViewChangeHistory(Vec<bool>);

    fn record_view_change(
        view: Res<benilla_world::view::ViewDistance>,
        mut history: ResMut<ViewChangeHistory>,
    ) {
        history.0.push(view.is_changed());
    }

    #[test]
    fn ads_near_clip_only_invalidates_scene_on_transitions() {
        let mut app = App::new();
        app.insert_resource(benilla_world::schedule::WorldLive(true));
        app.insert_resource(GuestInputPublisher {
            path: Some(PathBuf::from("test-input")),
            buttons: INPUT_AIM,
            ..default()
        });
        app.init_resource::<benilla_world::view::ViewDistance>()
            .init_resource::<CodcraftNearClip>()
            .init_resource::<ViewChangeHistory>()
            .add_systems(Update, (tune_ads_near_clip, record_view_change).chain());

        app.update(); // enter ADS: the near plane really changes
        app.update(); // remain in ADS: no scene-wide invalidation
        app.world_mut()
            .resource_mut::<GuestInputPublisher>()
            .buttons = 0;
        app.update(); // leave ADS: restore the original near plane
        app.update(); // remain out of ADS: no second invalidation

        let history = &app.world().resource::<ViewChangeHistory>().0;
        assert_eq!(history, &[true, false, true, false]);
        assert_eq!(
            app.world()
                .resource::<benilla_world::view::ViewDistance>()
                .nearclip,
            benilla_world::view::NEARCLIP_DEFAULT
        );
    }
}
