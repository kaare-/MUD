//! Matcap library + cavity shading for clay chunks.
//!
//! Clay is drawn with an unlit matcap (view-space normal → texture)
//! rather than lit PBR. Four procedural matcaps ship in-memory (no
//! image assets). Crevice darkening is baked into mesh UV.x at extract
//! time (`sculpt_core::cavity_brightness`) and scaled by
//! [`AppSettings::cavity_strength`].

use bevy::asset::load_internal_asset;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderRef, TextureDimension, TextureFormat,
};

use crate::settings::AppSettings;

const MATCAP_SHADER_HANDLE: Handle<Shader> =
    Handle::weak_from_u128(0x4d55_445f_4d41_5443_4150_5f53_4841_4445);

const MATCAP_SIZE: u32 = 256;

/// Built-in matcap presets (procedural).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum MatcapPreset {
    #[default]
    Clay = 0,
    SoftGrey = 1,
    Porcelain = 2,
    Slate = 3,
}

impl MatcapPreset {
    pub fn label(self) -> &'static str {
        match self {
            MatcapPreset::Clay => "Clay",
            MatcapPreset::SoftGrey => "Soft grey",
            MatcapPreset::Porcelain => "Porcelain",
            MatcapPreset::Slate => "Slate",
        }
    }

    pub fn all() -> [MatcapPreset; 4] {
        [
            MatcapPreset::Clay,
            MatcapPreset::SoftGrey,
            MatcapPreset::Porcelain,
            MatcapPreset::Slate,
        ]
    }
}

/// Unlit matcap material for clay chunk meshes.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct MatcapMaterial {
    /// `.x` = cavity strength (`0..=1`); remaining components pad to 16 bytes.
    #[uniform(0)]
    cavity: Vec4,
    #[texture(1)]
    #[sampler(2)]
    matcap: Handle<Image>,
}

impl MatcapMaterial {
    pub fn new(matcap: Handle<Image>, cavity_strength: f32) -> Self {
        Self {
            cavity: Vec4::new(cavity_strength.clamp(0.0, 1.0), 0.0, 0.0, 0.0),
            matcap,
        }
    }

    pub fn set_cavity_strength(&mut self, strength: f32) {
        self.cavity.x = strength.clamp(0.0, 1.0);
    }

    pub fn set_matcap(&mut self, matcap: Handle<Image>) {
        self.matcap = matcap;
    }
}

impl Material for MatcapMaterial {
    fn fragment_shader() -> ShaderRef {
        MATCAP_SHADER_HANDLE.into()
    }
}

/// Runtime handles for the shared clay matcap material + textures.
#[derive(Resource)]
pub struct MatcapState {
    pub material: Handle<MatcapMaterial>,
    textures: [Handle<Image>; 4],
}

impl MatcapState {
    pub fn texture(&self, preset: MatcapPreset) -> Handle<Image> {
        self.textures[preset as usize].clone()
    }
}

pub fn plugin(app: &mut App) {
    load_internal_asset!(
        app,
        MATCAP_SHADER_HANDLE,
        "shaders/matcap.wgsl",
        Shader::from_wgsl
    );
    app.add_plugins(MaterialPlugin::<MatcapMaterial>::default());
    app.add_systems(Startup, setup_matcaps);
    app.add_systems(Update, sync_matcap_settings);
}

/// Build procedural matcaps and the shared clay material. Must run
/// before workpiece spawn (registered earlier in `main`).
pub fn setup_matcaps(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<MatcapMaterial>>,
    settings: Res<AppSettings>,
) {
    let textures = [
        images.add(generate_matcap(MatcapPreset::Clay)),
        images.add(generate_matcap(MatcapPreset::SoftGrey)),
        images.add(generate_matcap(MatcapPreset::Porcelain)),
        images.add(generate_matcap(MatcapPreset::Slate)),
    ];
    let material = materials.add(MatcapMaterial::new(
        textures[settings.matcap as usize].clone(),
        settings.cavity_strength,
    ));
    commands.insert_resource(MatcapState {
        material,
        textures,
    });
}

/// Keep the shared clay material in sync with Preferences / View menu.
pub fn sync_matcap_settings(
    settings: Res<AppSettings>,
    matcap_state: Option<Res<MatcapState>>,
    mut materials: ResMut<Assets<MatcapMaterial>>,
) {
    let Some(matcap_state) = matcap_state else {
        return;
    };
    if !settings.is_changed() {
        return;
    }
    let Some(material) = materials.get_mut(matcap_state.material.id()) else {
        return;
    };
    material.set_matcap(matcap_state.texture(settings.matcap));
    material.set_cavity_strength(settings.cavity_strength);
}

fn generate_matcap(preset: MatcapPreset) -> Image {
    let size = MATCAP_SIZE;
    let mut data = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let v = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let r2 = u * u + v * v;
            let idx = ((y * size + x) * 4) as usize;
            if r2 > 1.0 {
                data[idx] = 20;
                data[idx + 1] = 20;
                data[idx + 2] = 22;
                data[idx + 3] = 255;
                continue;
            }
            let nz = (1.0 - r2).max(0.0).sqrt();
            let n = glam::Vec3::new(u, v, nz).normalize();
            let light = glam::Vec3::new(0.35, 0.55, 0.85).normalize();
            let ndotl = n.dot(light).clamp(0.0, 1.0);
            let hemi = 0.35 + 0.65 * (0.5 + 0.5 * n.y);
            let fresnel = (1.0 - n.z).clamp(0.0, 1.0).powf(3.0);
            let (base, spec_boost) = match preset {
                MatcapPreset::Clay => ([0.78f32, 0.55, 0.42], 0.08),
                MatcapPreset::SoftGrey => ([0.72, 0.72, 0.74], 0.06),
                MatcapPreset::Porcelain => ([0.92, 0.90, 0.88], 0.22),
                MatcapPreset::Slate => ([0.38, 0.42, 0.48], 0.18),
            };
            let diffuse = hemi * (0.55 + 0.45 * ndotl);
            let spec = ndotl.powf(48.0) * spec_boost + fresnel * spec_boost * 0.35;
            let rgb = [
                (base[0] * diffuse + spec).clamp(0.0, 1.0),
                (base[1] * diffuse + spec).clamp(0.0, 1.0),
                (base[2] * diffuse + spec).clamp(0.0, 1.0),
            ];
            data[idx] = (rgb[0] * 255.0) as u8;
            data[idx + 1] = (rgb[1] * 255.0) as u8;
            data[idx + 2] = (rgb[2] * 255.0) as u8;
            data[idx + 3] = 255;
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::linear();
    image
}
