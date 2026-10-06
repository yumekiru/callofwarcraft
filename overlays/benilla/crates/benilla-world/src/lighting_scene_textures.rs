//! Bounded mip-zero snapshots of the original image bytes; no albedo/light baking.
use benilla_formats::{BlpMipChain, BlpTexels};
use bevy::{
    image::{ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::render_resource::{TextureDimension, TextureFormat},
};
use std::{collections::HashMap, sync::Arc};

const MAX_TEXTURE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;

#[derive(Clone, Debug)]
pub struct TextureSnapshot {
    pub width: u32,
    pub height: u32,
    /// Original colour encoding and real alpha, decoded to RGBA8; never linearized here.
    /// In particular Warcraft BC Unorm images still hold authored gamma colour bytes.
    pub rgba: Arc<Vec<u8>>,
    pub source_format: TextureFormat,
    /// None retains the ImagePlugin global sampler policy rather than guessing its wrap mode.
    pub sampler: Option<ImageSamplerDescriptor>,
}

#[derive(Default)]
pub(super) struct TextureCache {
    entries: HashMap<AssetId<Image>, Arc<TextureSnapshot>>,
    bytes: usize,
}

impl TextureCache {
    pub(super) fn invalidate(&mut self, id: AssetId<Image>) {
        if let Some(snapshot) = self.entries.remove(&id) {
            self.bytes -= snapshot.rgba.len();
        }
    }

    pub(super) fn get(&mut self, id: AssetId<Image>, image: &Image) -> Option<Arc<TextureSnapshot>> {
        if let Some(snapshot) = self.entries.get(&id) {
            return Some(snapshot.clone());
        }
        let bytes = decoded_bytes(image)?;
        if self.entries.len() >= MAX_CACHE_ENTRIES || self.bytes + bytes > MAX_CACHE_BYTES {
            return None;
        }
        let snapshot = Arc::new(snapshot(image)?);
        self.bytes += bytes;
        self.entries.insert(id, snapshot.clone());
        Some(snapshot)
    }

    pub(super) fn retain<T>(&mut self, used: &HashMap<AssetId<Image>, T>) {
        self.entries.retain(|id, _| used.contains_key(id));
        self.bytes = self.entries.values().map(|s| s.rgba.len()).sum();
    }
}

fn decoded_bytes(image: &Image) -> Option<usize> {
    let size = image.texture_descriptor.size;
    if image.texture_descriptor.dimension != TextureDimension::D2
        || size.depth_or_array_layers != 1
        || size.width == 0
        || size.height == 0
        || size.width > 4096
        || size.height > 4096
    {
        return None;
    }
    let bytes = (size.width as usize)
        .checked_mul(size.height as usize)?
        .checked_mul(4)?;
    (bytes <= MAX_TEXTURE_BYTES).then_some(bytes)
}

fn snapshot(image: &Image) -> Option<TextureSnapshot> {
    let expected_rgba = decoded_bytes(image)?;
    let width = image.texture_descriptor.size.width;
    let height = image.texture_descriptor.size.height;
    let format = image.texture_descriptor.format;
    let source = image.data.as_deref()?;
    let texels = match format {
        TextureFormat::Rgba8Unorm
        | TextureFormat::Rgba8UnormSrgb
        | TextureFormat::Bgra8Unorm
        | TextureFormat::Bgra8UnormSrgb => BlpTexels::Rgba8Unorm,
        TextureFormat::Bc1RgbaUnorm | TextureFormat::Bc1RgbaUnormSrgb => BlpTexels::Bc1,
        TextureFormat::Bc2RgbaUnorm | TextureFormat::Bc2RgbaUnormSrgb => BlpTexels::Bc2,
        TextureFormat::Bc3RgbaUnorm | TextureFormat::Bc3RgbaUnormSrgb => BlpTexels::Bc3,
        _ => return None,
    };
    // Image.data concatenates mips; the decoder must receive exactly one mip-zero level.
    let level = source.get(..texels.level_bytes(width, height))?.to_vec();
    let mut decoded = BlpMipChain {
        width,
        height,
        texels,
        mips: vec![level],
    }
    .into_rgba8()
    .mips
    .remove(0);
    if decoded.len() != expected_rgba {
        return None;
    }
    if matches!(
        format,
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb
    ) {
        for pixel in decoded.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
    }
    Some(TextureSnapshot {
        width,
        height,
        rgba: Arc::new(decoded),
        source_format: format,
        sampler: match &image.sampler {
            ImageSampler::Default => None,
            ImageSampler::Descriptor(descriptor) => Some(descriptor.clone()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(format: TextureFormat, bytes: Vec<u8>) -> Image {
        let mut image = Image::new_fill(
            bevy::render::render_resource::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0; 4],
            TextureFormat::Rgba8Unorm,
            bevy::asset::RenderAssetUsages::default(),
        );
        image.texture_descriptor.format = format;
        image.data = Some(bytes);
        image
    }
    #[test]
    fn rgba_bgra_alpha_and_mip_zero_are_preserved() {
        let mut bytes = [20, 40, 60, 80].repeat(16);
        bytes.extend([255; 16]);
        let snapshot = snapshot(&image(TextureFormat::Bgra8UnormSrgb, bytes)).unwrap();
        assert_eq!(snapshot.rgba.len(), 64);
        assert_eq!(&snapshot.rgba[..4], &[60, 40, 20, 80]);
        assert_eq!(snapshot.source_format, TextureFormat::Bgra8UnormSrgb);
        assert!(snapshot.sampler.is_none());
    }
    #[test]
    fn real_bc1_bc2_bc3_alpha_decodes_without_tail_mips() {
        let mut bc1 = vec![0; 8];
        bc1[4..].fill(255); // c0 <= c1 and selector 3: transparent BC1 texels.
        let bc2 = vec![0; 16]; // explicit zero alpha.
        let bc3 = vec![0; 16]; // both interpolated alpha endpoints zero.
        for (format, mut bytes) in [
            (TextureFormat::Bc1RgbaUnorm, bc1),
            (TextureFormat::Bc2RgbaUnorm, bc2),
            (TextureFormat::Bc3RgbaUnorm, bc3),
        ] {
            bytes.extend([255; 16]);
            let snapshot = snapshot(&image(format, bytes)).unwrap();
            assert_eq!(snapshot.rgba.len(), 64);
            assert!(snapshot.rgba.chunks_exact(4).all(|p| p[3] == 0));
            assert_eq!(snapshot.source_format, format);
        }
    }
    #[test]
    fn absent_short_unsupported_and_large_data_are_unavailable() {
        assert!(snapshot(&image(TextureFormat::Bc3RgbaUnorm, vec![0; 15])).is_none());
        assert!(snapshot(&image(TextureFormat::Rgba16Float, vec![0; 128])).is_none());
        let mut large = image(TextureFormat::Rgba8Unorm, vec![0; 64]);
        large.texture_descriptor.size.width = 4096;
        large.texture_descriptor.size.height = 4096;
        assert!(snapshot(&large).is_none());
        large.data = None;
        assert!(snapshot(&large).is_none());
    }
    #[test]
    fn cache_reuses_arcs_and_invalidates_changed_images() {
        let id = Handle::<Image>::default().id();
        let mut cache = TextureCache::default();
        let source = image(TextureFormat::Rgba8Unorm, vec![10; 64]);
        let first = cache.get(id, &source).unwrap();
        let second = cache.get(id, &source).unwrap();
        assert!(Arc::ptr_eq(&first.rgba, &second.rgba));
        cache.invalidate(id);
        assert_eq!(cache.bytes, 0);
        let changed = cache
            .get(id, &image(TextureFormat::Rgba8Unorm, vec![20; 64]))
            .unwrap();
        assert_eq!(changed.rgba[0], 20);
        assert_eq!(first.rgba[0], 10);
    }
}
