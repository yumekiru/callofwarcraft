//! Missile-camera enemy designation brackets. UI only; no targeting/damage authority.
use super::*;

pub(super) fn plugin(app: &mut App) {
    app.add_systems(PostUpdate, boxes.after(bevy::transform::TransformSystems::Propagate));
}

fn boxes(
    state: Res<Predator>,
    cameras: Query<(&Camera, &GlobalTransform), With<benilla_world::view::WorldCamera>>,
    units: Query<(&crate::net::Guid, &crate::net::NetEntity, &crate::net::ObjectStore, &Transform), Without<crate::net::SelfPlayer>>,
    own: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
    reactions: crate::target::ReactionInputs,
    mut commands: Commands,
    mut nodes: Query<&mut Node>,
    mut active: Local<HashMap<u64, Entity>>,
) {
    let mut seen = std::collections::HashSet::new();
    if state.flying() {
        if let Ok((camera, frame))=cameras.single() {
            if let Some(size)=camera.logical_viewport_size() {
                for (guid, entity, store, transform) in &units {
                    if seen.len()>=96 { break; }
                    if entity.kind!=benilla_protocol::EntityKind::Unit || store.0.is_dead_or_ghost() ||
                        !crate::target::can_attack(Some(store),reactions.factions.as_deref(),&reactions.reputations,own.single().ok()) { continue; }
                    // Project a world-space body envelope each render frame; do
                    // not drive brackets from low-rate server movement samples.
                    let radius=store.0.unit_bounding_radius().clamp(0.35,2.0);
                    let height=(store.0.unit_combat_reach()*1.3).clamp(1.0,4.0);
                    let right=frame.right().as_vec3()*radius;
                    let mut low=Vec2::splat(f32::INFINITY);
                    let mut high=Vec2::splat(f32::NEG_INFINITY);
                    let mut valid=true;
                    for offset in [-right,right,Vec3::Y*height-right,Vec3::Y*height+right] {
                        if let Ok(p)=camera.world_to_viewport(frame,transform.translation+offset) { low=low.min(p);high=high.max(p); }
                        else { valid=false;break; }
                    }
                    if !valid || high.x<0.0 || high.y<0.0 || low.x>size.x || low.y>size.y { continue; }
                    let center=(low+high)*0.5;
                    let extent=(high-low+Vec2::splat(6.0)).clamp(Vec2::splat(16.0),Vec2::splat(180.0));
                    let node=Node { position_type:PositionType::Absolute,left:Val::Px(center.x-extent.x*0.5),
                        top:Val::Px(center.y-extent.y*0.5),width:Val::Px(extent.x),height:Val::Px(extent.y),..default() };
                    if let Some(&entity)=active.get(&guid.0) {
                        if let Ok(mut existing)=nodes.get_mut(entity) { *existing=node; }
                    } else {
                        let root=commands.spawn((node,GlobalZIndex(1049),bevy::ui::FocusPolicy::Pass))
                            .with_children(|parent| {
                                // Thin red corner brackets, matching the CoD
                                // remote-missile designation convention.
                                for (left,top) in [(true,true),(false,true),(true,false),(false,false)] {
                                    parent.spawn((Node {position_type:PositionType::Absolute,
                                        left:if left {Val::Px(0.0)} else {Val::Auto},right:if left {Val::Auto} else {Val::Px(0.0)},
                                        top:if top {Val::Px(0.0)} else {Val::Auto},bottom:if top {Val::Auto} else {Val::Px(0.0)},
                                        width:Val::Px(8.0),height:Val::Px(8.0),
                                        border:UiRect {left:Val::Px(if left {1.5} else {0.0}),right:Val::Px(if left {0.0} else {1.5}),
                                            top:Val::Px(if top {1.5} else {0.0}),bottom:Val::Px(if top {0.0} else {1.5})},..default()},
                                        BorderColor::all(Color::srgb(1.0,0.12,0.06)),bevy::ui::FocusPolicy::Pass));
                                }
                            }).id();
                        active.insert(guid.0,root);
                    }
                    seen.insert(guid.0);
                }
            }
        }
    }
    active.retain(|guid,entity| {
        if seen.contains(guid) {true} else {commands.entity(*entity).try_despawn();false}
    });
}
