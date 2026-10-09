use super::*;
use std::sync::{Arc,Mutex};

#[derive(Resource,Default)]
pub(crate) struct Placement {
    pub(crate) request: bool,
    active: bool,
    ready: bool,
    pending_until: f32,
    ghost: Vec<Entity>,
    green: Option<Handle<StandardMaterial>>,
    red: Option<Handle<StandardMaterial>>,
}
impl Placement {
    pub(crate) fn busy(&self) -> bool { self.request || self.active }
}

struct Flight {
    owner:u64, sequence:u32, position:Vec3, velocity:Vec3, received:f32,
    entities:Vec<Entity>, rotation:Quat,
}
#[derive(Resource,Default)]
struct Sentries {
    flights:Vec<Flight>,
    parts:Vec<(Handle<Mesh>,Handle<benilla_assets::materials::WowModelMaterial>)>,
    fingerprint:Option<u64>,
    next_load:f32, next_heartbeat:f32,
    mailbox:Option<Arc<Mutex<Option<super::PoseWire>>>>,
    ended:VecDeque<(u64,u32,f32)>,
}

pub(super) fn plugin(app:&mut App) {
    use crate::net::NetHandlerApp;
    app.init_resource::<Sentries>().init_resource::<Placement>()
        .net_handler(benilla_protocol::SessionEventKind::CodcraftFrag,receive)
        .add_systems(Update,(load_art,tick,place).chain());
}
fn receive(
    In(event):In<benilla_protocol::SessionEvent>,time:Res<Time>,
    mut state:ResMut<Sentries>,mut shots:ResMut<tracers::ExternalShots>,
    mut effects:ResMut<effects::Effects>,
    mut placement:ResMut<Placement>,self_guid:Res<crate::net::SelfGuid>,
    mut markers:MessageWriter<CodcraftHitMarker>,mut impacts:MessageWriter<CodcraftBulletImpact>,
) {
    let benilla_protocol::SessionEvent::CodcraftFrag{unit,sequence,phase,position,velocity,..}=event else {return};
    if !(7..=10).contains(&phase) || !position.into_iter().chain(velocity).all(f32::is_finite) {return;}
    if phase==10 {
        if self_guid.0==Some(unit) { markers.write(CodcraftHitMarker);impacts.write(CodcraftBulletImpact); }
        return;
    }
    let now=time.elapsed_secs();
    if phase==7 && self_guid.0==Some(unit) && placement.pending_until>now {
        placement.active=false;
        placement.pending_until=0.0;
    }
    state.ended.retain(|(_,_,expiry)|*expiry>now);
    if state.ended.iter().any(|(u,s,_)|*u==unit && *s==sequence) {return;}
    let key=(unit as u32).wrapping_mul(0x9e3779b9)^sequence;
    let position=benilla_assets::coords::wow_to_bevy(position);
    let vector=benilla_assets::coords::wow_to_bevy(velocity);
    if phase==9 {
        if shots.0.len()<64 {shots.0.push_back((position,vector,now));}
        effects.helicopter(9,key,position);
        return;
    }
    if let Some(f)=state.flights.iter_mut().find(|f|f.owner==unit && f.sequence==sequence) {
        f.position=position; f.velocity=vector; f.received=if phase==8 {-1000.0} else {now};
    } else if phase==7 && state.flights.len()<32 {
        state.flights.push(Flight{owner:unit,sequence,position,velocity:vector,received:now,entities:Vec::new(),rotation:Quat::IDENTITY});
    }
    if phase==8 {
        state.ended.push_back((unit,sequence,now+3.0)); 
    }
}

fn place(
    mut commands:Commands,time:Res<Time>,live:Res<benilla_world::schedule::WorldLive>,
    player:Res<crate::player::Player>,state:Res<Sentries>,mut placement:ResMut<Placement>,
    cameras:Query<&Transform,With<benilla_world::view::WorldCamera>>,
    collision:benilla_world::collision::WorldCollision,
    mouse:Res<ButtonInput<MouseButton>>,keys:Res<ButtonInput<KeyCode>>,
    over_ui:Res<crate::ui_script::PointerOverUi>,net:Res<crate::net::NetCommands>,
    mut materials:ResMut<Assets<StandardMaterial>>,
    mut visuals:Query<(&mut Transform,&mut MeshMaterial3d<StandardMaterial>),Without<benilla_world::view::WorldCamera>>,
) {
    let now=time.elapsed_secs();
    if placement.request {
        placement.request=false;placement.active=!placement.active;
        placement.ready=false;placement.pending_until=0.0;
    }
    if !live.0 || !player.active || keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right) {
        placement.active=false;
    }
    if !placement.active {
        for e in placement.ghost.drain(..) { commands.entity(e).try_despawn(); }
        return;
    }
    if !mouse.pressed(MouseButton::Left) { placement.ready=true; }
    let Ok(camera)=cameras.single() else {return};
    let direction=*camera.forward();
    let horizontal=direction.with_y(0.0).normalize_or_zero();
    if horizontal.length_squared()<0.5 {return;}
    let candidate=player.pos+horizontal*3.0;
    let aimed=collision.ray_los(camera.translation,camera.forward(),5.0)
        .filter(|h|h.normal.y>0.65)
        .map(|h|camera.translation+direction*h.distance).unwrap_or(candidate);
    let origin=aimed.with_y(player.pos.y+2.0);
    let ground=collision.ray_los(origin,Dir3::NEG_Y,5.0);
    let position=ground.as_ref().map(|h|origin-Vec3::Y*h.distance).unwrap_or(aimed);
    let supported=ground.as_ref().is_some_and(|h|h.normal.y>0.65);
    let feet_ok=[-0.4,0.4].into_iter().all(|x|[-0.4,0.4].into_iter().all(|z| {
        collision.ray_los(position+Vec3::new(x,1.0,z),Dir3::NEG_Y,2.0)
            .is_some_and(|h|h.normal.y>0.65 && (h.distance-1.0).abs()<=0.6)
    }));
    let sight=position+Vec3::Y-camera.translation;
    let clear=Dir3::new(sight).ok().is_some_and(|dir|
        collision.ray_los(camera.translation,dir,sight.length()).is_none());
    let valid=supported && feet_ok && clear && position.distance(player.pos)<=6.0;
    if placement.green.is_none() {
        placement.green=Some(materials.add(StandardMaterial {
            base_color:Color::srgba(0.1,1.0,0.3,0.45),alpha_mode:AlphaMode::Blend,
            unlit:true,cull_mode:None,..default()
        }));
        placement.red=Some(materials.add(StandardMaterial {
            base_color:Color::srgba(1.0,0.1,0.1,0.45),alpha_mode:AlphaMode::Blend,
            unlit:true,cull_mode:None,..default()
        }));
    }
    let highlight=if valid {placement.green.clone().unwrap()} else {placement.red.clone().unwrap()};
    // Only the carried sentry is rendered before deployment. The ground target
    // remains a collision query, not a second copy of the model.
    let held=Transform::from_translation(camera.translation+direction*0.9-*camera.up()*0.65+*camera.right()*0.3)
        .with_rotation(camera.rotation).with_scale(Vec3::splat(0.35));
    if placement.ghost.is_empty() && !state.parts.is_empty() {
        for (mesh,_) in &state.parts {
            let entity=commands.spawn((Name::new("Held Sentry Gun"),Mesh3d(mesh.clone()),MeshMaterial3d(highlight.clone()),held,
                bevy::camera::visibility::NoFrustumCulling)).id();
            placement.ghost.push(entity);
        }
    }
    for &entity in &placement.ghost {
        if let Ok((mut tf,mut mat))=visuals.get_mut(entity) { *tf=held;mat.0=highlight.clone(); }
    }
    if valid && !state.parts.is_empty() && placement.ready && !over_ui.0 && mouse.just_pressed(MouseButton::Left) && now>=placement.pending_until {
        if net.0.send(crate::net::ClientCommand::CodcraftSentry{position:benilla_assets::coords::bevy_to_wow(position)}).is_ok() {
            placement.pending_until=now+2.0;
        }
    }
}
fn load_art(
    time:Res<Time>,paths:Option<Res<ViewmodelPaths>>,mut state:ResMut<Sentries>,
    mut commands:Commands,mut images:ResMut<Assets<Image>>,mut meshes:ResMut<Assets<Mesh>>,
    mut batch:benilla_world::model_render::M2BatchMaterials,
    cameras:Query<Entity,With<benilla_world::view::WorldCamera>>,
) {
    let Some(paths)=paths else {return};
    if state.mailbox.is_none() {
        let mailbox:Arc<Mutex<Option<super::PoseWire>>>=Default::default();
        let weak=Arc::downgrade(&mailbox); let path=paths.model.with_extension("sentrypose");
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
    // Static exports are published once. Keep the pose until all render
    // dependencies are ready, including when no sentry is currently deployed.
    let mailbox=state.mailbox.as_ref().unwrap().clone();
    let Ok(mut pending)=mailbox.try_lock() else {return};
    let pose=pending.as_ref();
    let Some(pose)=pose else {return};
    if state.fingerprint!=Some(pose.fingerprint) && time.elapsed_secs()>=state.next_load {
        state.next_load=time.elapsed_secs()+0.5;
        let Ok(model)=read_packet(&paths.model.with_extension("sentrymesh"),MODEL_MAGIC).and_then(|b|parse_model(&b)) else {return};
        if model.fingerprint!=pose.fingerprint || model.uvs.len()!=pose.positions.len() {return;}
        let Ok(camera)=cameras.single() else {return};
        let Ok((entities,handles,materials))=install_model(&mut commands,&mut images,&mut meshes,&mut batch,camera,&model) else {return};
        for e in entities {commands.entity(e).try_despawn();}
        state.parts=handles.into_iter().zip(materials).collect(); state.fingerprint=Some(pose.fingerprint);
        info!("CoDCraft: installed native Sentry Gun geometry");
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
    player:Res<crate::player::Player>,paths:Option<Res<ViewmodelPaths>>,
    mut state:ResMut<Sentries>,mut commands:Commands,mut transforms:Query<&mut Transform>,
    mut effects:ResMut<effects::Effects>,
) {
    let now=time.elapsed_secs(); let parts=state.parts.clone();
    let heartbeat=now>=state.next_heartbeat;
    if heartbeat {state.next_heartbeat=now+0.5;}
    state.flights.retain_mut(|f| {
        let key=(f.owner as u32).wrapping_mul(0x9e3779b9)^f.sequence;
        if !live.0 || now-f.received>5.0 {
            for e in f.entities.drain(..) {commands.entity(e).try_despawn();}
             return false;
        }
        let position=f.position;
        if f.velocity.with_y(0.0).length_squared()>0.01 {
            let target=Transform::default().looking_to(f.velocity.with_y(0.0),Vec3::Y).rotation;
            f.rotation=f.rotation.slerp(target,(time.delta_secs()*5.0).min(1.0));
        }
        if f.entities.is_empty() {
            for (mesh,material) in &parts {
                f.entities.push(commands.spawn((Name::new("MW2 Sentry Gun"),Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),Transform::from_translation(position).with_rotation(f.rotation),Visibility::Visible,
                    bevy::camera::visibility::NoFrustumCulling)).id());
            }
        }
        for e in &f.entities {if let Ok(mut t)=transforms.get_mut(*e) {t.translation=position; t.rotation=f.rotation;}}
        
        true
    });
    if heartbeat && !state.flights.is_empty() {
        if let Some(paths)=paths {let _=std::fs::write(paths.model.with_extension("sentry-active"),b"active");}
    }
}
