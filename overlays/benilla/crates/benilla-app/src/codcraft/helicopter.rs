use super::*;
use std::sync::{Arc,Mutex};

struct Flight {
    owner:u64, sequence:u32, position:Vec3, velocity:Vec3, received:f32,
    entities:Vec<Entity>, rotation:Quat,
}
#[derive(Resource,Default)]
struct Helicopters {
    flights:Vec<Flight>,
    parts:Vec<(Handle<Mesh>,Handle<benilla_assets::materials::WowModelMaterial>)>,
    fingerprint:Option<u64>,
    next_load:f32, next_heartbeat:f32,
    mailbox:Option<Arc<Mutex<Option<super::PoseWire>>>>,
    ended:VecDeque<(u64,u32,f32)>,
}

pub(super) fn plugin(app:&mut App) {
    use crate::net::NetHandlerApp;
    app.init_resource::<Helicopters>()
        .net_handler(benilla_protocol::SessionEventKind::CodcraftFrag,receive)
        .add_systems(Update,(load_art,tick).chain());
}
fn receive(
    In(event):In<benilla_protocol::SessionEvent>,time:Res<Time>,
    mut state:ResMut<Helicopters>,mut shots:ResMut<tracers::ExternalShots>,
    mut effects:ResMut<effects::Effects>,
) {
    let benilla_protocol::SessionEvent::CodcraftFrag{unit,sequence,phase,position,velocity,..}=event else {return};
    if !(4..=6).contains(&phase) || !position.into_iter().chain(velocity).all(f32::is_finite) {return;}
    let now=time.elapsed_secs();
    state.ended.retain(|(_,_,expiry)|*expiry>now);
    if state.ended.iter().any(|(u,s,_)|*u==unit && *s==sequence) {return;}
    let key=(unit as u32).wrapping_mul(0x9e3779b9)^sequence;
    let position=benilla_assets::coords::wow_to_bevy(position);
    let vector=benilla_assets::coords::wow_to_bevy(velocity);
    if phase==6 {
        if shots.0.len()<64 {shots.0.push_back((position,vector,now));}
        effects.helicopter(8,key,position);
        return;
    }
    if let Some(f)=state.flights.iter_mut().find(|f|f.owner==unit && f.sequence==sequence) {
        f.position=position; f.velocity=vector; f.received=if phase==5 {-1000.0} else {now};
    } else if phase==4 && state.flights.len()<32 {
        state.flights.push(Flight{owner:unit,sequence,position,velocity:vector,received:now,entities:Vec::new(),rotation:Quat::IDENTITY});
    }
    if phase==5 {
        state.ended.push_back((unit,sequence,now+3.0)); effects.helicopter(7,key,position);
    }
}
fn load_art(
    time:Res<Time>,paths:Option<Res<ViewmodelPaths>>,mut state:ResMut<Helicopters>,
    mut commands:Commands,mut images:ResMut<Assets<Image>>,mut meshes:ResMut<Assets<Mesh>>,
    mut batch:benilla_world::model_render::M2BatchMaterials,
    cameras:Query<Entity,With<benilla_world::view::WorldCamera>>,
) {
    let Some(paths)=paths else {return};
    if state.mailbox.is_none() {
        let mailbox:Arc<Mutex<Option<super::PoseWire>>>=Default::default();
        let weak=Arc::downgrade(&mailbox); let path=paths.model.with_extension("helicopterpose");
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
    let pose=state.mailbox.as_ref().and_then(|m|m.try_lock().ok()?.take());
    let Some(pose)=pose else {return};
    if state.fingerprint!=Some(pose.fingerprint) && time.elapsed_secs()>=state.next_load {
        state.next_load=time.elapsed_secs()+0.5;
        let Ok(model)=read_packet(&paths.model.with_extension("helicoptermesh"),MODEL_MAGIC).and_then(|b|parse_model(&b)) else {return};
        if model.fingerprint!=pose.fingerprint || model.uvs.len()!=pose.positions.len() {return;}
        let Ok(camera)=cameras.single() else {return};
        let Ok((entities,handles,materials))=install_model(&mut commands,&mut images,&mut meshes,&mut batch,camera,&model) else {return};
        for e in entities {commands.entity(e).try_despawn();}
        state.parts=handles.into_iter().zip(materials).collect(); state.fingerprint=Some(pose.fingerprint);
        info!("CoDCraft: installed native Attack Helicopter geometry");
    }
    if state.fingerprint==Some(pose.fingerprint) && !state.flights.is_empty() {
        for (handle,_) in &state.parts {
            if let Some(mesh)=meshes.get_mut(handle) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION,pose.positions.clone());
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,pose.normals.clone());
            }
        }
    }
}
fn tick(
    time:Res<Time>,live:Res<benilla_world::schedule::WorldLive>,
    player:Res<crate::player::Player>,paths:Option<Res<ViewmodelPaths>>,
    mut state:ResMut<Helicopters>,mut commands:Commands,mut transforms:Query<&mut Transform>,
    mut effects:ResMut<effects::Effects>,
) {
    let now=time.elapsed_secs(); let parts=state.parts.clone();
    let heartbeat=now>=state.next_heartbeat;
    if heartbeat {state.next_heartbeat=now+0.5;}
    state.flights.retain_mut(|f| {
        let key=(f.owner as u32).wrapping_mul(0x9e3779b9)^f.sequence;
        if !live.0 || now-f.received>5.0 {
            for e in f.entities.drain(..) {commands.entity(e).try_despawn();}
            effects.helicopter(7,key,f.position); return false;
        }
        let position=f.position+f.velocity*(now-f.received).clamp(0.0,0.15);
        if f.velocity.with_y(0.0).length_squared()>0.01 {
            let target=Transform::default().looking_to(f.velocity.with_y(0.0),Vec3::Y).rotation;
            f.rotation=f.rotation.slerp(target,(time.delta_secs()*5.0).min(1.0));
        }
        if f.entities.is_empty() {
            for (mesh,material) in &parts {
                f.entities.push(commands.spawn((Name::new("MW2 Attack Helicopter"),Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),Transform::from_translation(position).with_rotation(f.rotation),Visibility::Visible)).id());
            }
        }
        for e in &f.entities {if let Ok(mut t)=transforms.get_mut(*e) {t.translation=position; t.rotation=f.rotation;}}
        if heartbeat && player.pos.distance_squared(position)<62500.0 {effects.helicopter(6,key,position);}
        true
    });
    if heartbeat && !state.flights.is_empty() {
        if let Some(paths)=paths {let _=std::fs::write(paths.model.with_extension("helicopter-active"),b"active");}
    }
}
