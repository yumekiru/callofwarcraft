//! CoDCraft fork HUD: confirmed damage points toward the attacker's world position.
use super::*;

#[derive(Component)]
struct DamageArc(usize);
#[derive(Resource, Default)]
struct Directions {
    hits: Vec<(u64, Vec3, f32)>,
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<Directions>()
        .add_systems(Startup, setup)
        .add_systems(Update, update);
}

fn setup(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(0.0),
                height: Val::Px(0.0),
                ..default()
            },
            GlobalZIndex(1100),
            bevy::ui::FocusPolicy::Pass,
        ))
        .with_children(|root| {
            for i in 0..8 {
                root.spawn((
                    DamageArc(i),
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Px(54.0),
                        height: Val::Px(10.0),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                    Visibility::Hidden,
                    UiTransform::default(),
                    bevy::ui::FocusPolicy::Pass,
                ));
            }
        });
}

fn update(
    time: Res<Time>,
    world: Res<benilla_world::schedule::WorldLive>,
    input: Res<GuestInputPublisher>,
    player: Res<crate::player::Player>,
    index: Res<crate::net::GuidIndex>,
    positions: Query<&GlobalTransform>,
    cameras: Query<&GlobalTransform, With<benilla_world::view::WorldCamera>>,
    mut combat: ResMut<CodcraftCombatState>,
    mut state: ResMut<Directions>,
    mut arcs: Query<(
        &DamageArc,
        &mut Node,
        &mut UiTransform,
        &mut BackgroundColor,
        &mut Visibility,
    )>,
) {
    let now = time.elapsed_secs();
    while let Some((guid, born)) = combat.incoming_hits.pop_front() {
        let Some(position) = index
            .0
            .get(&guid)
            .and_then(|e| positions.get(*e).ok())
            .map(|g| g.translation())
        else {
            continue;
        };
        state.hits.retain(|(g, _, _)| *g != guid);
        state.hits.push((guid, position, born));
    }
    state.hits.retain(|(_, _, born)| now - *born < 0.9);
    let mut strength = [0.0f32; 8];
    if world.0 && input.owns_gameplay_controls() {
        if let Ok(camera) = cameras.single() {
            for (_, source, born) in &state.hits {
                let angle =
                    combat_math::bearing(*source - player.pos, *camera.forward(), *camera.right());
                let sector = (angle / std::f32::consts::FRAC_PI_4).round() as i32;
                let i = sector.rem_euclid(8) as usize;
                strength[i] = strength[i].max((1.0 - (now - *born) / 0.9).clamp(0.0, 1.0));
            }
        }
    } else {
        state.hits.clear();
    }
    for (arc, mut node, mut transform, mut color, mut visibility) in &mut arcs {
        let angle = arc.0 as f32 * std::f32::consts::FRAC_PI_4;
        node.left = Val::Px(145.0 * angle.sin() - 27.0);
        node.top = Val::Px(-145.0 * angle.cos() - 5.0);
        transform.rotation = Rot2::radians(angle);
        color.0 = Color::srgba(1.0, 0.035, 0.01, strength[arc.0]);
        *visibility = if strength[arc.0] > 0.0 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}
