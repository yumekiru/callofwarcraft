use super::*;
use std::sync::{Arc,Mutex};
#[path="bomber_drops.rs"]
mod drops;

struct Flight {
    owner:u64, sequence:u32, position:Vec3, velocity:Vec3, received:f32,
    entities:Vec<Entity>, rotation:Quat,
}
#[derive(Resource,Default)]
struct Bombers {
    flights:Vec<Flight>,
    parts:Vec<(Handle<Mesh>,Handle<benilla_assets::materials::WowModelMaterial>)>,
    fingerprint:Option<u64>,
    next_load:f32, next_heartbeat:f32,
    mailbox:Option<Arc<Mutex<Option<super::PoseWire>>>>,
    ended:VecDeque<(u64,u32,f32)>,
}

pub(super) fn plugin(app:&mut App) {
    drops::plugin(app);
    use crate::net::NetHandlerApp;
    app.init_resource::<Bombers>().init_resource::<Targeting>()
        .net_handler(benilla_protocol::SessionEventKind::CodcraftFrag,receive)
        .add_systems(Update,(load_art,tick,cast).chain());
}
fn receive(
    In(event):In<benilla_protocol::SessionEvent>,time:Res<Time>,
    mut state:ResMut<Bombers>,
    mut effects:ResMut<effects::Effects>,
    mut blasts:ResMut<ragdoll::BlastImpulses>,
    targets:Query<(&crate::net::Guid,&Transform),Without<crate::net::SelfPlayer>>,
    collision:benilla_world::collision::WorldCollision,
) {
    let benilla_protocol::SessionEvent::CodcraftFrag{unit,sequence,phase,position,velocity,..}=event else {return};
    if !(11..=13).contains(&phase) || !position.into_iter().chain(velocity).all(f32::is_finite) {return;}
    let now=time.elapsed_secs();
    state.ended.retain(|(_,_,expiry)|*expiry>now);
    if state.ended.iter().any(|(u,s,_)|*u==unit && *s==sequence) {return;}
    let key=(unit as u32).wrapping_mul(0x9e3779b9)^sequence;
    let position=benilla_assets::coords::wow_to_bevy(position);
    let vector=benilla_assets::coords::wow_to_bevy(velocity);
    if phase==13 {
        effects.bomber_explosion(position);
        for (guid,t) in &targets {
            let offset=t.translation+Vec3::Y-position;
            let distance=offset.length();
            if distance<12.0 {
                let clear=Dir3::new(offset).ok().is_some_and(|dir|
                    collision.ray_los(position+Vec3::Y*0.6,dir,(distance-0.6).max(0.0)).is_none());
                if clear {
                    let strength=(1.0-distance/12.0).max(0.0);
                    let velocity=offset.with_y(0.0).normalize_or_zero()*(5.0+5.0*strength)+Vec3::Y*(3.0+strength);
                    blasts.0.insert(guid.0,(now+2.0,velocity));
                }
            }
        }
        return;
    }
    if let Some(f)=state.flights.iter_mut().find(|f|f.owner==unit && f.sequence==sequence) {
        f.position=position; f.velocity=vector; f.received=if phase==12 {-1000.0} else {now};
    } else if phase==11 && state.flights.len()<32 {
        state.flights.push(Flight{owner:unit,sequence,position,velocity:vector,received:now,entities:Vec::new(),rotation:Quat::IDENTITY});
    }
    if phase==12 {
        state.ended.push_back((unit,sequence,now+3.0)); effects.helicopter(12,key,position);
    }
}
fn load_art(
    time:Res<Time>,paths:Option<Res<ViewmodelPaths>>,mut state:ResMut<Bombers>,
    mut commands:Commands,mut images:ResMut<Assets<Image>>,mut meshes:ResMut<Assets<Mesh>>,
    mut batch:benilla_world::model_render::M2BatchMaterials,
    cameras:Query<Entity,With<benilla_world::view::WorldCamera>>,
) {
    let Some(paths)=paths else {return};
    if state.mailbox.is_none() {
        let mailbox:Arc<Mutex<Option<super::PoseWire>>>=Default::default();
        let weak=Arc::downgrade(&mailbox); let path=paths.model.with_extension("bomberpose");
        std::thread::spawn(move|| {
            let mut last=None;
            loop {
                let Some(slot)=weak.upgrade() else {break};
                let modified=std::fs::metadata(&path).ok().and_then(|m|m.modified().ok());
                if modified.is_some() && modified!=last {
                    if let Ok(p)=read_packet(&path,POSE_MAGIC).and_then(|b|parse_pose(&b)) {
                        if let Ok(mut slot)=slot.lock() {*slot=Some(p); last=modified;}
                    }
                }
                drop(slot); std::thread::sleep(std::time::Duration::from_millis(16));
            }
        });
        state.mailbox=Some(mailbox);
    }
    let mailbox=state.mailbox.as_ref().unwrap().clone();
    let Ok(mut pending)=mailbox.try_lock() else {return};
    let pose=pending.as_ref();
    let Some(pose)=pose else {return};
    if state.fingerprint!=Some(pose.fingerprint) && time.elapsed_secs()>=state.next_load {
        state.next_load=time.elapsed_secs()+0.5;
        let Ok(model)=read_packet(&paths.model.with_extension("bombermesh"),MODEL_MAGIC).and_then(|b|parse_model(&b)) else {return};
        if model.fingerprint!=pose.fingerprint || model.uvs.len()!=pose.positions.len() {return;}
        let Ok(camera)=cameras.single() else {return};
        let Ok((entities,handles,materials))=install_model(&mut commands,&mut images,&mut meshes,&mut batch,camera,&model) else {return};
        for e in entities {commands.entity(e).try_despawn();}
        state.parts=handles.into_iter().zip(materials).collect(); state.fingerprint=Some(pose.fingerprint);
        info!("CoDCraft: installed native Stealth Bomber geometry");
    }
    if state.fingerprint==Some(pose.fingerprint) {
        for (handle,_) in &state.parts {
            if let Some(mesh)=meshes.get_mut(handle) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION,pose.positions.clone());
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,pose.normals.clone());
            }
        }
        *pending=None;
    }
}
fn tick(
    time:Res<Time>,live:Res<benilla_world::schedule::WorldLive>,
    player:Res<crate::player::Player>,
    mut state:ResMut<Bombers>,mut commands:Commands,mut transforms:Query<&mut Transform>,
    mut effects:ResMut<effects::Effects>,
) {
    let now=time.elapsed_secs(); let parts=state.parts.clone();
    let heartbeat=now>=state.next_heartbeat;
    if heartbeat {state.next_heartbeat=now+0.5;}
    state.flights.retain_mut(|f| {
        let key=(f.owner as u32).wrapping_mul(0x9e3779b9)^f.sequence;
        if !live.0 || now-f.received>5.0 {
            for e in f.entities.drain(..) {commands.entity(e).try_despawn();}
            effects.helicopter(12,key,f.position); return false;
        }
        let position=f.position+f.velocity*(now-f.received).clamp(0.0,0.15);
        if f.velocity.with_y(0.0).length_squared()>0.01 {
            let target=Transform::default().looking_to(f.velocity.with_y(0.0),Vec3::Y).rotation;
            f.rotation=target;
        }
        if f.entities.is_empty() {
            for (mesh,material) in &parts {
                f.entities.push(commands.spawn((Name::new("MW2 Stealth Bomber"),Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),Transform::from_translation(position).with_rotation(f.rotation),Visibility::Visible,
                    bevy::camera::visibility::NoFrustumCulling)).id());
            }
        }
        for e in &f.entities {if let Ok(mut t)=transforms.get_mut(*e) {t.translation=position; t.rotation=f.rotation;}}
        if heartbeat && !f.entities.is_empty() && player.pos.distance_squared(position)<62500.0 {effects.helicopter(11,key,position);}
        true
    });
}
#[derive(Resource,Default)]
pub(crate) struct Targeting {pub(crate) request:bool}
fn cast(
    mut target:ResMut<Targeting>,state:Res<Bombers>,live:Res<benilla_world::schedule::WorldLive>,
    player:Res<crate::player::Player>,self_guid:Res<crate::net::SelfGuid>,
    cameras:Query<&Transform,With<benilla_world::view::WorldCamera>>,
    collision:benilla_world::collision::WorldCollision,net:Res<crate::net::NetCommands>,
) {
    if !std::mem::take(&mut target.request) || !live.0 || !player.active {return;}
    if state.parts.is_empty() {
        warn!("CoDCraft: Stealth Bomber native geometry not ready; cast refused");return;
    }
    if state.flights.iter().any(|f|Some(f.owner)==self_guid.0) {return;}
    let Ok(camera)=cameras.single() else {return};
    let forward=*camera.forward();
    let horizontal=forward.with_y(0.0).normalize_or_zero();
    if horizontal.length_squared()<0.5 {return;}
    let centre=collision.ray_los(camera.translation,camera.forward(),85.0)
        .map(|hit|camera.translation+forward*hit.distance).unwrap_or(player.pos+horizontal*35.0);
    let origin=centre+Vec3::Y*2.0;
    let Some(hit)=collision.ray_los(origin,Dir3::NEG_Y,100.0) else {return};
    let point=origin-Vec3::Y*hit.distance;
    if point.distance(player.pos)>90.0 {return;}
    let facing=benilla_assets::coords::bevy_to_wow(horizontal);
    let heading=facing[1].atan2(facing[0]);
    let _=net.0.send(crate::net::ClientCommand::CodcraftBomber{
        position:benilla_assets::coords::bevy_to_wow(point),heading,
    });
}

