//! Validate both ends when deferred reparenting executes, not when it is queued.
use bevy::ecs::{hierarchy::ChildOf, system::Commands, world::World, entity::Entity};

pub(super) fn reparent(commands: &mut Commands, child: Entity, parent: Entity) {
    commands.queue(move |world: &mut World| {
        if world.get_entity(parent).is_err() { return; }
        if let Ok(mut entity) = world.get_entity_mut(child) {
            entity.insert(ChildOf(parent));
        }
    });
}
