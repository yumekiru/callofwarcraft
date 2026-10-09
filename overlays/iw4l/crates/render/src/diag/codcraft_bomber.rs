use super::*;

#[derive(Resource, Default)]
pub(super) struct Bomber {
    next: f32,
    fingerprint: Option<u64>,
    name: Option<String>,
    inspected: bool,
}

pub(super) fn publish(
    time: Res<Time>, catalog: Option<Res<assets::PreparedProjectileMeshes>>,
    materials: Res<render_anim::anim::model_materials::PreparedModelMaterials>,
    tess: Option<Res<render_scene::TessMaterials>>, images: Res<Assets<Image>>,
    sounds:Option<Res<audio::SoundBank>>,
    mut state: ResMut<Bomber>,
) {
    let Some(paths)=paths() else { return };
    if time.elapsed_secs()<state.next { return; }
    state.next=time.elapsed_secs()+1.0/30.0;
    let (Some(catalog),Some(tess))=(catalog,tess) else { return };
    if !state.inspected {
        let available=(0..catalog.0.len()).filter_map(|i| catalog.0.get_at(i))
            .filter(|m| { let n=m.skel.name.to_lowercase(); n.contains("bomber") || n.contains("b2") || n.contains("stealth") })
            .map(|m| format!("{} bones={} vertices={}\n{}",m.skel.name,m.skel.bone_names.len(),m.skel.positions.len(),m.skel.bone_names.join(" "))).collect::<Vec<_>>();
        let audio=sounds.as_ref().map(|b|b.0.sounds.iter().filter(|s|s.name.contains("bomber") || s.name.contains("stealth") || s.name.contains("b2") || s.name.contains("airstrike")).map(|s|s.name.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
        let _=std::fs::write(paths.model.with_extension("bomber-catalog.txt"),format!("{}\nSounds:\n{}",available.join("\n"),audio));
        state.inspected=true;
    }
    if state.name.is_none() {
        let mut candidates=(0..catalog.0.len()).filter_map(|i| catalog.0.get_at(i))
            .filter(|m| { let n=m.skel.name.to_lowercase();
                n=="vehicle_b2_bomber" &&
                !["destroy","wreck","crash","debris","gib","cockpit","dead","viewmodel"].iter().any(|s| n.contains(s)) &&
                !m.skel.positions.is_empty() })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|m| (!m.skel.name.contains("b2"),m.skel.name.clone()));
        state.name=candidates.first().map(|m| m.skel.name.clone());
    }
    let Some(model)=state.name.as_ref().and_then(|name|(0..catalog.0.len()).filter_map(|i|catalog.0.get_at(i)).find(|m| &m.skel.name==name)) else { return };
    let mut hash=std::collections::hash_map::DefaultHasher::new(); model.skel.name.hash(&mut hash);
    2u32.hash(&mut hash);
    model.skel.positions.len().hash(&mut hash); let fingerprint=hash.finish();
    let export=state.fingerprint!=Some(fingerprint);
    // Static native bind geometry is shared across every placed bomber.
    if !export { return; }
    let decoded=(|| -> Option<(Vec<u8>,Vec<u8>)> {
        let dobj=xmodel_runtime::DObj::build(&[(model.skel.pose.as_ref()?,None)]).ok()?;
        let request=xmodel_runtime::DObjPoseRequest::bind_pose();
        let (surfaces,_)=render_anim::occupancy::script_model::pose_script_dobj_controlled(
            None,&[&model.skel],&dobj,&request,None,&[],|_,_,_| {})?;
        let mut rows=Vec::new(); let mut groups=Vec::new();
        for surface in surfaces {
            if export {
                let authored=model.material_edges.get(surface.surface_index)?.bound_index()?;
                let material=materials.projectile_material(&tess.catalog,assets::MaterialIndex::from_order(authored))?.clone();
                let maps=render_scene::runtime_maps(Some(assets::MaterialIndex::from_order(authored)),&tess.catalog,tess.material_images.as_ref());
                let image=images.get(maps.color.as_ref()?)?;
                let texture=asset_material::decoded_image_top_level_rgba8(image).ok()?;
                let bits=tess.catalog.ordinal_for_asset_id(assets::MaterialIndex::from_order(authored))
                    .and_then(|i|tess.catalog.material_for_sorted_ordinal(i.get()))
                    .and_then(|m|m.state_bits_table.first().copied());
                let alpha=bits.map(super::codcraft_fx::effect_alpha).unwrap_or_else(||alpha_code(&material));
                // Rotor blur cards retain coverage in the image even if the
                // flattened colour pass has lost its authored blend state.
                let alpha=if alpha==0 && texture.2.chunks_exact(4).any(|p|p[3]<255) {2} else {alpha};
                diag::info!(World,"CoDCraft: bomber material={} alpha={} native_bits={:?}",model.material_present_name(surface.surface_index).unwrap_or("?"),alpha,bits);
                let base=rows.len() as u32;
                let indices=surface.mesh.indices()?.iter().map(|i|base+i as u32).collect::<Vec<_>>();
                groups.push((material,image,texture,indices,alpha));
            }
            rows.extend_from_slice(&surface.packed_vertices);
        }
        if rows.is_empty() { return None; }
        let mut mesh=Vec::new();
        if export {
            push_u64(&mut mesh,fingerprint); push_u32(&mut mesh,rows.len() as u32); push_u32(&mut mesh,groups.len() as u32);
            for row in &rows {
                push_vec(&mut mesh,asset_model::unpack_packed_tex_coords(packed_u32(row,20)));
                push_vec(&mut mesh,asset_model::unpack_color(packed_u32(row,16)));
            }
            for (material,image,(width,height,rgba),indices,alpha) in groups {
                push_u32(&mut mesh,alpha); push_u32(&mut mesh,1); push_f32(&mut mesh,alpha_cutoff(&material));
                push_u32(&mut mesh,u32::from(image.texture_descriptor.format.is_srgb()));
                push_u32(&mut mesh,width); push_u32(&mut mesh,height); push_u32(&mut mesh,rgba.len() as u32); mesh.extend_from_slice(&rgba);
                push_u32(&mut mesh,indices.len() as u32); for i in indices { push_u32(&mut mesh,i); }
            }
        }
        let mut pose=Vec::new(); push_u64(&mut pose,fingerprint); push_u32(&mut pose,1); push_u32(&mut pose,rows.len() as u32);
        for v in Mat4::IDENTITY.to_cols_array() { push_f32(&mut pose,v); }
        for row in rows {
            let p:[f32;3]=core::array::from_fn(|i|f32::from_le_bytes(row[i*4..i*4+4].try_into().unwrap()));
            let n=asset_model::unpack_unit_vec(packed_u32(&row,24));
            push_vec(&mut pose,[-p[1]/36.0,p[2]/36.0,-p[0]/36.0]); push_vec(&mut pose,[-n[1],n[2],-n[0]]);
        }
        Some((mesh,pose))
    })();
    if let Some((mesh,pose))=decoded {
        if export && write_packet(&paths.model.with_extension("bombermesh"),STATIC_MAGIC,&mesh).is_err() { return; }
        if write_packet(&paths.model.with_extension("bomberpose"),POSE_MAGIC,&pose).is_ok() && export {
            state.fingerprint=Some(fingerprint);
            diag::info!(World,"CoDCraft: exported native bomber gun {}",model.skel.name);
        }
    }
}
