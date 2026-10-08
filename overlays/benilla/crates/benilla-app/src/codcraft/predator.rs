//! Native IW4 Predator flight, presented in Warcraft and swept against its world.
use super::*;
use std::sync::{Arc, Mutex};
use std::time::Instant;
#[path = "predator_math.rs"]
mod math;
#[path = "predator_targets.rs"]
mod target_boxes;
use math::{Packet, decode, native};
type Mailbox = Arc<Mutex<Option<(Instant, Packet)>>>;
#[derive(Resource, Default)]
pub(crate) struct Predator {
    pub(crate) request: bool,
    sequence: u32,
    deadline: f32,
    packet: Option<(Instant, Packet)>,
    mailbox: Option<Mailbox>,
    rotation: Quat,
    offset: Vec3,
    cast_position: Vec3,
    cast_forward: Vec3,
    previous: Option<Vec3>,
    camera_position: Option<Vec3>,
    camera_rotation: Option<Quat>,
    ended: bool,
    impacted: bool,
}
impl Predator { pub(super) fn flying(&self) -> bool { self.deadline>0.0 && self.previous.is_some() && !self.ended } }
pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Predator>()
        .add_systems(Update, tick.before(super::publish_guest_input))
        .add_systems(Update, camera.after(crate::player::PlayerControlSet));
    target_boxes::plugin(app);
}
fn wall_us() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_micros() as u64) }
fn tick(
    time: Res<Time<Real>>, live: Res<benilla_world::schedule::WorldLive>, player: Res<crate::player::Player>,
    paths: Option<Res<ViewmodelPaths>>, net: Option<Res<crate::net::NetCommands>>,
    collision: benilla_world::collision::WorldCollision, mut state: ResMut<Predator>, mut input: ResMut<GuestInputPublisher>,
    mut effects: ResMut<effects::Effects>, mut combat: ResMut<CodcraftCombatState>,
    mut blasts: ResMut<ragdoll::BlastImpulses>, feedback: Res<Time>,
    targets: Query<(&crate::net::Guid, &Transform), Without<crate::net::SelfPlayer>>,
    self_store: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    keys: Res<ButtonInput<KeyCode>>,
    cameras: Query<&Transform, With<benilla_world::view::WorldCamera>>,
) {
    let now = time.elapsed_secs();
    let (Some(paths), Some(net), Some(input_path)) = (paths, net, input.path.clone()) else { return };
    if state.mailbox.is_none() {
        let mailbox: Mailbox = Default::default(); let weak = Arc::downgrade(&mailbox);
        let path = paths.model.with_extension("predator");
        std::thread::spawn(move || {
            let mut stamp=0;
            loop {
                let decoded = read_packet(&path, b"CCPR").ok().and_then(|b| decode(&b));
                let Some(mailbox)=weak.upgrade() else { break };
                if let Some(p)=decoded.filter(|p| p.stamp!=stamp && wall_us().saturating_sub(p.stamp)<500000) {
                    stamp=p.stamp; if let Ok(mut slot)=mailbox.lock() { *slot=Some((Instant::now(),p)); }
                }
                drop(mailbox); std::thread::sleep(std::time::Duration::from_millis(4));
            }
        });
        state.mailbox=Some(mailbox);
    }
    let received=state.mailbox.as_ref().and_then(|m| m.try_lock().ok()?.take());
    if let Some(p)=received { state.packet=Some(p); }
    let alive=live.0 && player.active && self_store.single().is_ok_and(|s| !s.0.is_dead_or_ghost());
    if state.request {
        state.request=false;
        if alive && state.deadline==0.0 {
            state.sequence=state.sequence.wrapping_add(1).max((wall_us()/1000000) as u32).max(1);
            let seq=state.sequence;
            if std::fs::write(input_path.with_extension("predator-command"),format!("CCPR1 {} {}",wall_us(),wall_us())).is_ok() {
                state.deadline=now+25.0; state.ended=false; state.impacted=false; state.previous=None;
                state.camera_position=None; state.camera_rotation=None; state.packet=None;
                state.cast_position=player.pos;
                state.cast_forward=cameras.single().map_or(Vec3::NEG_Z,|camera| *camera.forward());
                input.alt_interact=false;
                let _=net.0.send(crate::net::ClientCommand::CodcraftPredator { sequence:seq,phase:0,position:benilla_assets::coords::bevy_to_wow(player.pos) });
                info!("CoDCraft: Predator {seq} requested");
            }
        }
    }
    if state.deadline==0.0 { input.predator_control=false; return; }
    input.predator_control=true;
    if !alive || now>state.deadline || keys.just_pressed(KeyCode::Escape) { state.ended=true; }
    if let Some((received,p))=state.packet {
        if p.active && !state.ended {
            if received.elapsed().as_secs_f32()>0.75 { state.ended=true; }
            else {
                if state.previous.is_none() {
                    // The retail launch script uses a fixed map heading. Rotate
                    // its entire flight basis into the facing captured at cast,
                    // including steering, camera and inverse impact feedback.
                    state.rotation=math::flight_rotation(state.cast_forward,p.velocity,p.angles.y);
                    state.offset=math::cast_offset(state.cast_position,state.rotation,p.origin,p.player);
                    state.previous=Some(state.rotation*native(p.origin)+state.offset);
                    info!("CoDCraft: Predator native remote camera active entity={}",p.entity);
                }
                let age=(wall_us().saturating_sub(p.stamp) as f32/1000000.0).min(0.10);
                let next=state.rotation*native(p.origin+p.velocity*age)+state.offset;
                let previous=state.previous.unwrap(); let delta=next-previous;
                if let Some(distance)=collision.cast_camera(previous,delta,true) {
                    let impact=previous+delta.normalize_or_zero()*distance;
                    let native_impact=benilla_assets::coords::bevy_to_wow(state.rotation.inverse()*(impact-state.offset)).map(|x|x*36.0);
                    let _=std::fs::write(input_path.with_extension("predator-impact"),format!("CCPI1 {} {} {} {} {}",wall_us(),p.entity,native_impact[0],native_impact[1],native_impact[2]));
                    let _=net.0.send(crate::net::ClientCommand::CodcraftPredator { sequence:state.sequence,phase:1,position:benilla_assets::coords::bevy_to_wow(impact) });
                    effects.predator(p.weapon,impact);
                    for (guid,t) in &targets {
                        let offset=t.translation+Vec3::Y-impact;
                        if offset.length()<17.0 {
                            combat.queue_grenade(guid.0,feedback.elapsed_secs());
                            if let Ok(dir)=Dir3::new(offset) { if collision.ray_los(impact+Vec3::Y*0.15,dir,offset.length()).is_none() {
                                blasts.0.insert(guid.0,(feedback.elapsed_secs()+2.0,offset.normalize_or_zero()*9.0+Vec3::Y*5.0));
                            }}
                        }
                    }
                    state.impacted=true; state.ended=true;
                    info!("CoDCraft: Predator {} hit Warcraft terrain at {:?}",state.sequence,impact);
                } else {
                    state.previous=Some(next);
                    state.camera_position=Some(next);
                    let pitch=p.angles.x.to_radians(); let yaw=p.angles.y.to_radians();
                    let direction=state.rotation*native(Vec3::new(pitch.cos()*yaw.cos(),pitch.cos()*yaw.sin(),-pitch.sin())).normalize();
                    state.camera_rotation=Some(Transform::default().looking_to(direction,Vec3::Y).rotation);
                }
            }
        } else if !p.active && state.previous.is_some() { state.ended=true; }
    }
    if state.ended {
        if let Some((_,p))=state.packet.filter(|(_,p)|p.active && !state.impacted) {
            let _=std::fs::write(input_path.with_extension("predator-cancel"),format!("CCPC1 {} {}",wall_us(),p.entity));
        }
        state.deadline=0.0; state.previous=None; state.camera_position=None; state.camera_rotation=None;
        input.predator_control=false;
        // Close failed/cancelled flights too, without fabricating a damage event.
        let _=net.0.send(crate::net::ClientCommand::CodcraftPredator { sequence:state.sequence,phase:2,position:benilla_assets::coords::bevy_to_wow(player.pos) });
    }
}
fn camera(state: Res<Predator>, mut cameras: Query<(&mut Transform, &mut Projection), With<benilla_world::view::WorldCamera>>, mut saved: Local<Option<Projection>>) {
    let Ok((mut camera,mut projection))=cameras.single_mut() else { return };
    if let (Some(position),Some(rotation))=(state.camera_position,state.camera_rotation) {
        if saved.is_none() { *saved=Some(projection.clone()); }
        if let Projection::Perspective(p)=&mut *projection { p.far=2000.0; p.fov=70.0_f32.to_radians(); }
        camera.translation=position; camera.rotation=rotation;
    } else if let Some(original)=saved.take() {
        *projection=original;
    }
}
