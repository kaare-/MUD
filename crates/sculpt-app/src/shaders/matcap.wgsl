#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

@group(2) @binding(0) var<uniform> cavity_params: vec4<f32>;
@group(2) @binding(1) var matcap_texture: texture_2d<f32>;
@group(2) @binding(2) var matcap_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let n_world = normalize(in.world_normal);
    // View-space normal → classic matcap UV.
    let n_view = normalize((view.view_from_world * vec4(n_world, 0.0)).xyz);
    let uv = n_view.xy * 0.5 + vec2(0.5, 0.5);
    var color = textureSample(matcap_texture, matcap_sampler, uv).rgb;

    // Cavity brightness packed in UV.x at extract time (1 = open, <1 = crevice).
#ifdef VERTEX_UVS_A
    let cavity = clamp(in.uv.x, 0.0, 1.0);
#else
    let cavity = 1.0;
#endif
    let strength = clamp(cavity_params.x, 0.0, 1.0);
    let shade = mix(1.0, cavity, strength);
    color *= shade;

    return vec4(color, 1.0);
}
