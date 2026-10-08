//! Shared tested decode/coordinate contract for native Predator observations.
use bevy::prelude::*;
#[derive(Clone, Copy)]
pub(super) struct Packet { pub stamp: u64, pub active: bool, pub weapon: u32, pub entity: u32, pub origin: Vec3, pub velocity: Vec3, pub angles: Vec3, pub player: Vec3 }
pub(super) fn decode(b: &[u8]) -> Option<Packet> {
    if b.len()!=68 { return None; }
    let u=|at|u32::from_le_bytes(b[at..at+4].try_into().unwrap());
    if u(8)>1 { return None; }
    let v=|at|Vec3::new(f32::from_bits(u(at)),f32::from_bits(u(at+4)),f32::from_bits(u(at+8)));
    let p=Packet { stamp:u64::from_le_bytes(b[..8].try_into().unwrap()), active:u(8)==1, weapon:u(12),entity:u(16),origin:v(20),velocity:v(32),angles:v(44),player:v(56) };
    (p.origin.is_finite() && p.velocity.is_finite() && p.angles.is_finite() && p.player.is_finite() && p.velocity.length()<20000.0).then_some(p)
}
pub(super) fn native(v:Vec3)->Vec3 { Vec3::new(-v.y,v.z,-v.x)/36.0 }
pub(super) fn flight_rotation(facing:Vec3,velocity:Vec3,native_yaw:f32)->Quat {
    let desired=facing.with_y(0.0).normalize_or_zero();
    let mut initial=native(velocity).with_y(0.0).normalize_or_zero();
    if initial.length_squared()<0.5 {
        let yaw=native_yaw.to_radians();
        initial=native(Vec3::new(yaw.cos(),yaw.sin(),0.0)).normalize_or_zero();
    }
    if desired.length_squared()<0.5 { return Quat::IDENTITY; }
    let heading=|v:Vec3|(-v.x).atan2(-v.z);
    Quat::from_rotation_y(heading(desired)-heading(initial))
}
pub(super) fn cast_offset(cast:Vec3,rotation:Quat,missile:Vec3,player:Vec3)->Vec3 {
    let origin=rotation*native(missile);
    // Anchor horizontal coordinates to this cast, not IW4 map/spawn coordinates.
    // Preserve native altitude above the grounded guest player.
    let height=(missile.z-player.z)/36.0;
    cast+Vec3::Y*height-origin
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn native_launch_is_rotated_to_every_cast_facing_without_changing_descent() {
        for heading in [0.0_f32,0.8,1.5707964,3.1415927,-1.5707964] {
            let desired=Quat::from_rotation_y(heading)*Vec3::NEG_Z;
            for velocity in [Vec3::new(1000.0,0.0,-2000.0),Vec3::new(0.0,-1000.0,-2000.0),Vec3::new(0.0,0.0,-2000.0)] {
                let rotation=flight_rotation(desired,velocity,0.0);
                let reference=if velocity.x==0.0 && velocity.y==0.0 { native(Vec3::X) } else { native(velocity) };
                let mapped=(rotation*reference).with_y(0.0).normalize();
                assert!(mapped.dot(desired)>0.99999);
                assert!(((rotation*native(velocity)).y-native(velocity).y).abs()<0.0001);
            }
        }
    }
    #[test] fn launch_is_centered_on_cast_even_far_from_guest_spawn() {
        let cast=Vec3::new(700.0,30.0,-1200.0);
        for yaw in [0.0,1.7,-2.8] {
            let rotation=Quat::from_rotation_y(yaw);
            let missile=Vec3::new(-20000.0,10000.0,3900.0);
            let offset=cast_offset(cast,rotation,missile,Vec3::new(100.0,200.0,300.0));
            let actual=rotation*native(missile)+offset;
            assert!((actual-(cast+Vec3::Y*100.0)).length()<0.001);
        }
    }
    #[test] fn native_packet_is_strict_and_does_not_accept_nan_or_partial_writes() {
        let mut b=vec![0;68]; b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[20..24].copy_from_slice(&720.0f32.to_le_bytes());
        assert_eq!(decode(&b).unwrap().origin.x,720.0);
        assert!(decode(&b[..67]).is_none());
        b[32..36].copy_from_slice(&f32::NAN.to_le_bytes()); assert!(decode(&b).is_none());
        b[32..36].copy_from_slice(&30000.0f32.to_le_bytes()); assert!(decode(&b).is_none());
        b[32..36].copy_from_slice(&0f32.to_le_bytes()); b[8..12].copy_from_slice(&2u32.to_le_bytes()); assert!(decode(&b).is_none());
    }
    #[test] fn grounded_player_mapping_preserves_native_missile_height_and_rotation() {
        let player=Vec3::new(1000.0,2000.0,300.0);
        let anchor=Vec3::new(-20.0,15.0,40.0);
        let rotation=Quat::from_rotation_y(1.7);
        let offset=anchor-rotation*native(player);
        assert!((rotation*native(player)+offset-anchor).length()<0.00001);
        let missile=player+Vec3::Z*3600.0;
        let host=rotation*native(missile)+offset;
        assert!((host.y-anchor.y-100.0).abs()<0.0001);
        let inverse=rotation.inverse()*(host-offset)*36.0;
        let restored=Vec3::new(-inverse.z,-inverse.x,inverse.y);
        assert!((restored-missile).length()<0.01);
    }
}
