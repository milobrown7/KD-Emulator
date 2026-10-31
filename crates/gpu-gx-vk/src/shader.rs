//! GLSL → SPIR-V compilation (naga, pure Rust — no shaderc/glslang on the
//! device), plus the vendor-specific compute shaders (texture decoding,
//! EFB-to-RAM) and the TEV uber-shader used while specialised pipelines
//! compile in the background.

use kd_core::thermal::GpuVendorProfile;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

#[derive(Debug)]
pub struct ShaderError(pub String);

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ShaderError {}

/// Compile GLSL 450 to validated SPIR-V words.
pub fn compile(src: &str, stage: Stage) -> Result<Vec<u32>, ShaderError> {
    let naga_stage = match stage {
        Stage::Vertex => naga::ShaderStage::Vertex,
        Stage::Fragment => naga::ShaderStage::Fragment,
        Stage::Compute => naga::ShaderStage::Compute,
    };
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(naga_stage), src)
        .map_err(|e| ShaderError(format!("GLSL parse error: {e:?}")))?;
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .map_err(|e| ShaderError(format!("validation error: {e:?}")))?;
    let options = naga::back::spv::Options::default();
    let pipeline = naga::back::spv::PipelineOptions { shader_stage: naga_stage, entry_point: "main".into() };
    naga::back::spv::write_vec(&module, &info, &options, Some(&pipeline))
        .map_err(|e| ShaderError(format!("SPIR-V error: {e:?}")))
}

/// SPIR-V opcodes that terminate a fragment (`discard`).
pub const OP_KILL: u32 = 252;
pub const OP_TERMINATE_INVOCATION: u32 = 4416;
pub const OP_DEMOTE_TO_HELPER: u32 = 5380;
const OP_EXECUTION_MODE: u32 = 16;
const EXEC_MODE_LOCAL_SIZE: u32 = 17;

/// Iterate SPIR-V instructions as (opcode, operands).
pub fn instructions(spv: &[u32]) -> impl Iterator<Item = (u32, &[u32])> {
    let mut i = 5; // skip header
    std::iter::from_fn(move || {
        let word = *spv.get(i)?;
        let len = (word >> 16) as usize;
        let op = word & 0xFFFF;
        if len == 0 || i + len > spv.len() {
            return None;
        }
        let operands = &spv[i + 1..i + len];
        i += len;
        Some((op, operands))
    })
}

/// Does the module contain any fragment-terminating instruction?
pub fn contains_kill(spv: &[u32]) -> bool {
    instructions(spv).any(|(op, _)| matches!(op, OP_KILL | OP_TERMINATE_INVOCATION | OP_DEMOTE_TO_HELPER))
}

/// The compute shader's declared `LocalSize`, if any.
pub fn local_size(spv: &[u32]) -> Option<[u32; 3]> {
    instructions(spv).find_map(|(op, a)| {
        (op == OP_EXECUTION_MODE && a.len() >= 5 && a[1] == EXEC_MODE_LOCAL_SIZE).then(|| [a[2], a[3], a[4]])
    })
}

/// Compute shader that decodes RGB5A3 4x4 blocks from a storage buffer of
/// big-endian texels into an RGBA8 storage image. One invocation per texel
/// along X; the workgroup width follows the vendor profile.
pub fn texture_decode_rgb5a3(profile: GpuVendorProfile) -> String {
    let lx = profile.compute_local_size_x();
    let mut s = String::new();
    let _ = write!(
        s,
        r#"#version 450
layout(local_size_x = {lx}, local_size_y = 1, local_size_z = 1) in;
layout(std430, set = 0, binding = 0) readonly buffer Src {{ uint words[]; }} src;
layout(rgba8, set = 0, binding = 1) writeonly uniform image2D dst;
layout(push_constant) uniform Params {{ uint width; uint height; }} params;

vec4 decode5a3(uint v) {{
    if ((v & 0x8000u) != 0u) {{
        return vec4(float((v >> 10u) & 31u) / 31.0, float((v >> 5u) & 31u) / 31.0, float(v & 31u) / 31.0, 1.0);
    }}
    return vec4(float((v >> 8u) & 15u) / 15.0, float((v >> 4u) & 15u) / 15.0, float(v & 15u) / 15.0, float((v >> 12u) & 7u) / 7.0);
}}

void main() {{
    uint texel = gl_GlobalInvocationID.x;
    uint total = params.width * params.height;
    if (texel >= total) {{ return; }}
    uint block = texel / 16u;
    uint within = texel % 16u;
    uint blocks_x = (params.width + 3u) / 4u;
    uint x = (block % blocks_x) * 4u + within % 4u;
    uint y = (block / blocks_x) * 4u + within / 4u;
    uint word = src.words[texel / 2u];
    // big-endian halfword: byte-swap the 16-bit lane
    uint half_le = (texel % 2u == 0u) ? (word & 0xFFFFu) : (word >> 16u);
    uint v = ((half_le & 0xFFu) << 8u) | (half_le >> 8u);
    if (x < params.width && y < params.height) {{
        imageStore(dst, ivec2(int(x), int(y)), decode5a3(v));
    }}
}}
"#
    );
    s
}

/// EFB → RAM copy scaler: reads the RGBA8 EFB and writes RGB565 texels,
/// with the vendor workgroup width.
pub fn efb_to_ram_rgb565(profile: GpuVendorProfile) -> String {
    let lx = profile.compute_local_size_x();
    format!(
        r#"#version 450
layout(local_size_x = {lx}, local_size_y = 1, local_size_z = 1) in;
layout(set = 0, binding = 0) uniform texture2D efb;
layout(set = 0, binding = 1) uniform sampler efb_sampler;
layout(std430, set = 0, binding = 2) buffer Dst {{ uint halves[]; }} dst;
layout(push_constant) uniform Params {{ uint width; uint height; float scale; }} params;

void main() {{
    uint i = gl_GlobalInvocationID.x;
    if (i >= params.width * params.height) {{ return; }}
    vec2 uv = (vec2(float(i % params.width), float(i / params.width)) + 0.5) / vec2(float(params.width), float(params.height));
    vec4 c = textureLod(sampler2D(efb, efb_sampler), uv, 0.0);
    uint r = uint(c.r * 31.0 + 0.5);
    uint g = uint(c.g * 63.0 + 0.5);
    uint b = uint(c.b * 31.0 + 0.5);
    dst.halves[i] = (r << 11u) | (g << 5u) | b;
}}
"#
    )
}

/// Vertex shader shared by all TEV pipelines (transforms are done in XF
/// on the CPU or in a compute pre-pass; this just forwards attributes).
pub fn passthrough_vertex() -> &'static str {
    r#"#version 450
layout(location = 0) in vec4 a_pos;
layout(location = 1) in vec4 a_color0;
layout(location = 2) in vec4 a_color1;
layout(location = 3) in vec2 a_tex0;
layout(location = 0) out vec4 v_color0;
layout(location = 1) out vec4 v_color1;
layout(location = 2) out vec2 v_tex0;
void main() {
    gl_Position = a_pos;
    v_color0 = a_color0;
    v_color1 = a_color1;
    v_tex0 = a_tex0;
}
"#
}

/// Uber-shader: interprets up to 16 TEV stages from a uniform block. Used
/// when thermal pressure (or the user) enables asynchronous shaders, so no
/// pipeline is ever compiled on the draw path.
pub fn uber_fragment(preserve_hsr: bool) -> String {
    let alpha = if preserve_hsr {
        // HSR-preserving variant: alpha testing is done by blending the
        // fragment out instead of discarding.
        "    o_color = vec4(result.rgb, pass ? result.a : 0.0);\n"
    } else {
        "    if (!pass) { discard; }\n    o_color = result;\n"
    };
    format!(
        r#"#version 450
layout(location = 0) in vec4 v_color0;
layout(location = 1) in vec4 v_color1;
layout(location = 2) in vec2 v_tex0;
layout(location = 0) out vec4 o_color;
layout(set = 0, binding = 0) uniform texture2D t_tex0;
layout(set = 0, binding = 1) uniform sampler s_tex0;
layout(set = 1, binding = 0) uniform UberBlock {{
    uvec4 env[16];      // x = colour env, y = alpha env, z = order, w = ksel
    vec4 regs[4];
    vec4 konst[4];
    uvec4 misc;         // x = stages, y = alpha test register
}} u;

vec3 cin(uint sel, vec4 r[4], vec4 tex, vec4 ras, vec3 kc) {{
    switch (sel) {{
        case 0u: return r[0].rgb; case 1u: return vec3(r[0].a);
        case 2u: return r[1].rgb; case 3u: return vec3(r[1].a);
        case 4u: return r[2].rgb; case 5u: return vec3(r[2].a);
        case 6u: return r[3].rgb; case 7u: return vec3(r[3].a);
        case 8u: return tex.rgb;  case 9u: return vec3(tex.a);
        case 10u: return ras.rgb; case 11u: return vec3(ras.a);
        case 12u: return vec3(1.0); case 13u: return vec3(0.5);
        case 14u: return kc;
        default: return vec3(0.0);
    }}
}}

float ain(uint sel, vec4 r[4], vec4 tex, vec4 ras, float ka) {{
    switch (sel) {{
        case 0u: return r[0].a; case 1u: return r[1].a; case 2u: return r[2].a; case 3u: return r[3].a;
        case 4u: return tex.a; case 5u: return ras.a; case 6u: return ka;
        default: return 0.0;
    }}
}}

float scale_of(uint s) {{ return s == 3u ? 0.5 : float(1u << s); }}
float bias_of(uint b) {{ return b == 1u ? 0.5 : (b == 2u ? -0.5 : 0.0); }}

bool cmp(uint c, float a, float r) {{
    switch (c) {{
        case 0u: return false; case 1u: return a < r; case 2u: return a == r; case 3u: return a <= r;
        case 4u: return a > r; case 5u: return a != r; case 6u: return a >= r;
        default: return true;
    }}
}}

void main() {{
    vec4 r[4] = vec4[4](u.regs[0], u.regs[1], u.regs[2], u.regs[3]);
    for (uint i = 0u; i < u.misc.x; i++) {{
        uvec4 e = u.env[i];
        vec4 tex = ((e.z >> 6u) & 1u) != 0u ? texture(sampler2D(t_tex0, s_tex0), v_tex0) : vec4(1.0);
        uint ch = (e.z >> 7u) & 7u;
        vec4 ras = ch == 7u ? vec4(0.0) : ((ch & 1u) != 0u ? v_color1 : v_color0);
        vec3 kc = u.konst[(e.w >> 0u) & 3u].rgb;
        float ka = u.konst[(e.w >> 5u) & 3u].a;
        uint c = e.x;
        vec3 crgb = (cin(c & 15u, r, tex, ras, kc) * 0.0 + cin(c & 15u, r, tex, ras, kc))
                    + (((c >> 18u) & 1u) != 0u ? -1.0 : 1.0)
                      * mix(cin((c >> 12u) & 15u, r, tex, ras, kc), cin((c >> 8u) & 15u, r, tex, ras, kc), cin((c >> 4u) & 15u, r, tex, ras, kc));
        crgb = (crgb + bias_of((c >> 16u) & 3u)) * scale_of((c >> 20u) & 3u);
        crgb = ((c >> 19u) & 1u) != 0u ? clamp(crgb, 0.0, 1.0) : clamp(crgb, -4.0, 4.0);
        uint a = e.y;
        float aval = ain((a >> 4u) & 7u, r, tex, ras, ka)
                   + (((a >> 18u) & 1u) != 0u ? -1.0 : 1.0)
                     * mix(ain((a >> 13u) & 7u, r, tex, ras, ka), ain((a >> 10u) & 7u, r, tex, ras, ka), ain((a >> 7u) & 7u, r, tex, ras, ka));
        aval = (aval + bias_of((a >> 16u) & 3u)) * scale_of((a >> 20u) & 3u);
        aval = ((a >> 19u) & 1u) != 0u ? clamp(aval, 0.0, 1.0) : clamp(aval, -4.0, 4.0);
        uint cd = (c >> 22u) & 3u;
        uint ad = (a >> 22u) & 3u;
        r[cd] = vec4(crgb, r[cd].a);
        r[ad] = vec4(r[ad].rgb, aval);
    }}
    vec4 result = clamp(r[0], vec4(0.0), vec4(1.0));
    uint at = u.misc.y;
    float a8 = floor(result.a * 255.0 + 0.5);
    bool x = cmp((at >> 16u) & 7u, a8, float(at & 255u));
    bool y = cmp((at >> 19u) & 7u, a8, float((at >> 8u) & 255u));
    uint logic = (at >> 22u) & 3u;
    bool pass = logic == 0u ? (x && y) : (logic == 1u ? (x || y) : (logic == 2u ? (x != y) : (x == y)));
{alpha}}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tev::{AlphaTest, TevConfig};

    #[test]
    fn passthrough_tev_compiles_without_kill() {
        let src = crate::tev::generate_glsl(&TevConfig::passthrough());
        let spv = compile(&src, Stage::Fragment).unwrap_or_else(|e| panic!("{e}\n{src}"));
        assert_eq!(spv[0], 0x0723_0203, "SPIR-V magic");
        assert!(!contains_kill(&spv), "always-pass alpha test must not emit OpKill (PowerVR HSR)");
    }

    #[test]
    fn alpha_cutout_emits_kill() {
        let mut cfg = TevConfig::passthrough();
        cfg.alpha_test = AlphaTest::from_reg(128 | 6 << 16 | 7 << 19);
        let src = crate::tev::generate_glsl(&cfg);
        let spv = compile(&src, Stage::Fragment).unwrap_or_else(|e| panic!("{e}\n{src}"));
        assert!(contains_kill(&spv));
    }

    #[test]
    fn textured_multistage_tev_compiles() {
        let mut bp = [0u32; 256];
        bp[0x00] = 2 << 10 | 2; // 3 stages, 2 texgens
        bp[0x28] = (1 << 6) | (1 << 3) | (1 << 12 | 1 << 18 | 1 << 15); // stage0 tex0/coord1, stage1 tex1/coord1
        bp[0xC0] = 0xF8AF;
        bp[0xC1] = 0x7 << 4 | 4 << 7 | 5 << 10 | 7 << 13;
        bp[0xC2] = 0x0F80 | 0x2 << 22; // writes to C1
        bp[0xC4] = 0x80E2 | 1 << 19;
        bp[0xF3] = 7 << 16 | 7 << 19;
        bp[0xF6] = 0x10 << 4;
        let cfg = TevConfig::from_bp(&bp);
        let src = crate::tev::generate_glsl(&cfg);
        let spv = compile(&src, Stage::Fragment).unwrap_or_else(|e| panic!("{e}\n{src}"));
        assert!(!contains_kill(&spv));
    }

    #[test]
    fn compute_shaders_use_vendor_workgroup_sizes() {
        for (profile, lx) in [(GpuVendorProfile::PowerVrDxt, 32), (GpuVendorProfile::ArmMali, 64)] {
            for src in [texture_decode_rgb5a3(profile), efb_to_ram_rgb565(profile)] {
                let spv = compile(&src, Stage::Compute).unwrap_or_else(|e| panic!("{e}\n{src}"));
                assert_eq!(local_size(&spv), Some([lx, 1, 1]));
            }
        }
    }

    #[test]
    fn uber_shader_variants() {
        let hsr = compile(&uber_fragment(true), Stage::Fragment).unwrap_or_else(|e| panic!("{e}"));
        assert!(!contains_kill(&hsr), "PowerVR uber-shader keeps HSR");
        let plain = compile(&uber_fragment(false), Stage::Fragment).unwrap_or_else(|e| panic!("{e}"));
        assert!(contains_kill(&plain));
        compile(passthrough_vertex(), Stage::Vertex).unwrap();
    }
}
