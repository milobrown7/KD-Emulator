//! Texture Environment (TEV): configuration from BP registers, a CPU
//! evaluator used by the software renderer, and the GLSL generator that
//! feeds the SPIR-V compiler.

use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct StageEnv {
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub bias: u8,
    pub sub: bool,
    pub clamp: bool,
    pub scale: u8,
    pub dest: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Stage {
    pub color: StageEnv,
    pub alpha: StageEnv,
    pub texmap: u8,
    pub texcoord: u8,
    pub tex_enabled: bool,
    /// 0 = colour0, 1 = colour1, 7 = zero (GX_COLORNULL).
    pub channel: u8,
    pub kcsel: u8,
    pub kasel: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaComp {
    Never,
    Less,
    Equal,
    LEqual,
    Greater,
    NEqual,
    GEqual,
    Always,
}

impl AlphaComp {
    fn from_bits(v: u32) -> Self {
        [Self::Never, Self::Less, Self::Equal, Self::LEqual, Self::Greater, Self::NEqual, Self::GEqual, Self::Always][(v & 7) as usize]
    }
    fn eval(self, a: u8, r: u8) -> bool {
        match self {
            Self::Never => false,
            Self::Less => a < r,
            Self::Equal => a == r,
            Self::LEqual => a <= r,
            Self::Greater => a > r,
            Self::NEqual => a != r,
            Self::GEqual => a >= r,
            Self::Always => true,
        }
    }
    fn glsl(self) -> &'static str {
        match self {
            Self::Never => "false",
            Self::Less => "<",
            Self::Equal => "==",
            Self::LEqual => "<=",
            Self::Greater => ">",
            Self::NEqual => "!=",
            Self::GEqual => ">=",
            Self::Always => "true",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlphaTest {
    pub comp0: AlphaComp,
    pub comp1: AlphaComp,
    pub ref0: u8,
    pub ref1: u8,
    /// 0 AND, 1 OR, 2 XOR, 3 XNOR
    pub logic: u8,
}

impl AlphaTest {
    pub fn from_reg(v: u32) -> Self {
        Self {
            ref0: v as u8,
            ref1: (v >> 8) as u8,
            comp0: AlphaComp::from_bits(v >> 16),
            comp1: AlphaComp::from_bits(v >> 19),
            logic: ((v >> 22) & 3) as u8,
        }
    }

    pub fn passes(&self, a: u8) -> bool {
        let x = self.comp0.eval(a, self.ref0);
        let y = self.comp1.eval(a, self.ref1);
        match self.logic {
            0 => x && y,
            1 => x || y,
            2 => x != y,
            _ => x == y,
        }
    }

    /// True when the test can never reject a fragment. Such shaders must
    /// not contain `discard` (OpKill), or PowerVR disables Hidden Surface
    /// Removal for the whole pipeline.
    pub fn always_passes(&self) -> bool {
        (0..=255u8).all(|a| self.passes(a))
    }
}

/// Everything that determines the generated pixel shader.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TevConfig {
    pub stages: Vec<Stage>,
    pub alpha_test: AlphaTest,
    pub num_texgens: u8,
}

fn env(v: u32, color: bool) -> StageEnv {
    let (d, c, b, a) = if color {
        (v & 15, (v >> 4) & 15, (v >> 8) & 15, (v >> 12) & 15)
    } else {
        ((v >> 4) & 7, (v >> 7) & 7, (v >> 10) & 7, (v >> 13) & 7)
    };
    StageEnv {
        a: a as u8,
        b: b as u8,
        c: c as u8,
        d: d as u8,
        bias: ((v >> 16) & 3) as u8,
        sub: v & (1 << 18) != 0,
        clamp: v & (1 << 19) != 0,
        scale: ((v >> 20) & 3) as u8,
        dest: ((v >> 22) & 3) as u8,
    }
}

impl TevConfig {
    /// Build from the BP register file.
    pub fn from_bp(bp: &[u32; 256]) -> Self {
        let genmode = bp[0x00];
        let n = (((genmode >> 10) & 15) + 1) as usize;
        let stages = (0..n)
            .map(|s| {
                let order = bp[0x28 + s / 2] >> if s % 2 == 0 { 0 } else { 12 };
                let ksel = bp[0xF6 + s / 2] >> if s % 2 == 0 { 4 } else { 14 };
                Stage {
                    color: env(bp[0xC0 + s * 2], true),
                    alpha: env(bp[0xC1 + s * 2], false),
                    texmap: (order & 7) as u8,
                    texcoord: ((order >> 3) & 7) as u8,
                    tex_enabled: order & (1 << 6) != 0,
                    channel: ((order >> 7) & 7) as u8,
                    kcsel: (ksel & 31) as u8,
                    kasel: ((ksel >> 5) & 31) as u8,
                }
            })
            .collect();
        Self { stages, alpha_test: AlphaTest::from_reg(bp[0xF3]), num_texgens: (genmode & 15) as u8 }
    }

    /// A single stage that passes the rasterised colour through.
    pub fn passthrough() -> Self {
        let color = StageEnv { a: 15, b: 15, c: 15, d: 10, ..Default::default() }; // d = RASC
        let alpha = StageEnv { a: 7, b: 7, c: 7, d: 5, ..Default::default() }; // d = RASA
        Self {
            stages: vec![Stage { color, alpha, channel: 0, ..Default::default() }],
            alpha_test: AlphaTest::from_reg(7 << 16 | 7 << 19),
            num_texgens: 0,
        }
    }
}

/// Inputs to one pixel's TEV evaluation (values 0.0..=1.0).
#[derive(Debug, Clone, Copy, Default)]
pub struct TevInputs {
    pub ras: [[f32; 4]; 2],
    pub tex: [[f32; 4]; 8],
    pub regs: [[f32; 4]; 4],
    pub konst: [[f32; 4]; 4],
}

fn konst_color(sel: u8, k: &[[f32; 4]; 4]) -> [f32; 3] {
    match sel {
        0..=7 => {
            let v = 1.0 - sel as f32 / 8.0;
            [v, v, v]
        }
        12..=15 => {
            let c = k[(sel - 12) as usize];
            [c[0], c[1], c[2]]
        }
        16..=31 => {
            let c = k[((sel - 16) % 4) as usize];
            let v = c[((sel - 16) / 4) as usize];
            [v, v, v]
        }
        _ => [1.0, 1.0, 1.0],
    }
}

fn konst_alpha(sel: u8, k: &[[f32; 4]; 4]) -> f32 {
    match sel {
        0..=7 => 1.0 - sel as f32 / 8.0,
        16..=31 => k[((sel - 16) % 4) as usize][((sel - 16) / 4) as usize],
        _ => 1.0,
    }
}

fn apply(e: &StageEnv, a: f32, b: f32, c: f32, d: f32) -> f32 {
    let lerp = a * (1.0 - c) + b * c;
    let bias = match e.bias {
        1 => 0.5,
        2 => -0.5,
        _ => 0.0,
    };
    let scale = [1.0, 2.0, 4.0, 0.5][e.scale as usize];
    let v = (if e.sub { d - lerp } else { d + lerp } + bias) * scale;
    if e.clamp {
        v.clamp(0.0, 1.0)
    } else {
        v.clamp(-4.0, 4.0)
    }
}

/// Evaluate the TEV on the CPU. Returns RGBA and whether alpha test passes.
pub fn evaluate(cfg: &TevConfig, inp: &TevInputs) -> ([f32; 4], bool) {
    let mut regs = inp.regs; // [prev, c0, c1, c2]
    for s in &cfg.stages {
        let tex = if s.tex_enabled { inp.tex[s.texmap as usize] } else { [1.0; 4] };
        let ras = match s.channel {
            0 | 2 | 4 => inp.ras[0],
            1 | 3 | 5 => inp.ras[1],
            _ => [0.0; 4],
        };
        let kc = konst_color(s.kcsel, &inp.konst);
        let ka = konst_alpha(s.kasel, &inp.konst);
        let csrc = |sel: u8, ch: usize| -> f32 {
            match sel {
                0 => regs[0][ch],
                1 => regs[0][3],
                2 => regs[1][ch],
                3 => regs[1][3],
                4 => regs[2][ch],
                5 => regs[2][3],
                6 => regs[3][ch],
                7 => regs[3][3],
                8 => tex[ch],
                9 => tex[3],
                10 => ras[ch],
                11 => ras[3],
                12 => 1.0,
                13 => 0.5,
                14 => kc[ch],
                _ => 0.0,
            }
        };
        let asrc = |sel: u8| -> f32 {
            match sel {
                0 => regs[0][3],
                1 => regs[1][3],
                2 => regs[2][3],
                3 => regs[3][3],
                4 => tex[3],
                5 => ras[3],
                6 => ka,
                _ => 0.0,
            }
        };
        let e = &s.color;
        let rgb: [f32; 3] = std::array::from_fn(|ch| apply(e, csrc(e.a, ch), csrc(e.b, ch), csrc(e.c, ch), csrc(e.d, ch)));
        let ea = &s.alpha;
        let a = apply(ea, asrc(ea.a), asrc(ea.b), asrc(ea.c), asrc(ea.d));
        let cd = e.dest as usize;
        regs[cd][..3].copy_from_slice(&rgb);
        regs[ea.dest as usize][3] = a;
    }
    let out = regs[0].map(|v| v.clamp(0.0, 1.0));
    let pass = cfg.alpha_test.passes((out[3] * 255.0).round() as u8);
    (out, pass)
}

/// GLSL (Vulkan dialect) fragment shader for a TEV configuration.
///
/// `discard` is only emitted when the alpha test can actually fail.
pub fn generate_glsl(cfg: &TevConfig) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str("#version 450\n");
    s.push_str("layout(location = 0) in vec4 v_color0;\nlayout(location = 1) in vec4 v_color1;\n");
    for i in 0..8 {
        let _ = writeln!(s, "layout(location = {}) in vec2 v_tex{i};", 2 + i);
    }
    s.push_str("layout(location = 0) out vec4 o_color;\n");
    s.push_str("layout(set = 1, binding = 0) uniform TevBlock { vec4 regs[4]; vec4 konst[4]; vec4 alpha_ref; } tev;\n");
    let used: Vec<u8> = {
        let mut v: Vec<u8> = cfg.stages.iter().filter(|st| st.tex_enabled).map(|st| st.texmap).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    for &m in &used {
        let _ = writeln!(s, "layout(set = 0, binding = {}) uniform texture2D t_tex{m};", m * 2);
        let _ = writeln!(s, "layout(set = 0, binding = {}) uniform sampler s_tex{m};", m * 2 + 1);
    }
    s.push_str("void main() {\n");
    s.push_str("    vec4 r0 = tev.regs[0]; vec4 r1 = tev.regs[1]; vec4 r2 = tev.regs[2]; vec4 r3 = tev.regs[3];\n");
    let reg = ["r0", "r1", "r2", "r3"];
    for (i, st) in cfg.stages.iter().enumerate() {
        let tex = if st.tex_enabled {
            format!("texture(sampler2D(t_tex{0}, s_tex{0}), v_tex{1})", st.texmap, st.texcoord)
        } else {
            "vec4(1.0)".to_string()
        };
        let ras = match st.channel {
            0 | 2 | 4 => "v_color0",
            1 | 3 | 5 => "v_color1",
            _ => "vec4(0.0)",
        };
        let kc = match st.kcsel {
            0..=7 => format!("vec3({:.4})", 1.0 - st.kcsel as f32 / 8.0),
            12..=15 => format!("tev.konst[{}].rgb", st.kcsel - 12),
            16..=31 => format!("vec3(tev.konst[{}][{}])", (st.kcsel - 16) % 4, (st.kcsel - 16) / 4),
            _ => "vec3(1.0)".into(),
        };
        let ka = match st.kasel {
            0..=7 => format!("{:.4}", 1.0 - st.kasel as f32 / 8.0),
            16..=31 => format!("tev.konst[{}][{}]", (st.kasel - 16) % 4, (st.kasel - 16) / 4),
            _ => "1.0".into(),
        };
        let _ = writeln!(s, "    {{ // stage {i}\n        vec4 tex = {tex}; vec4 ras = {ras}; vec3 kc = {kc}; float ka = {ka};");
        let cin = |sel: u8| -> String {
            match sel {
                0 => "r0.rgb".into(),
                1 => "vec3(r0.a)".into(),
                2 => "r1.rgb".into(),
                3 => "vec3(r1.a)".into(),
                4 => "r2.rgb".into(),
                5 => "vec3(r2.a)".into(),
                6 => "r3.rgb".into(),
                7 => "vec3(r3.a)".into(),
                8 => "tex.rgb".into(),
                9 => "vec3(tex.a)".into(),
                10 => "ras.rgb".into(),
                11 => "vec3(ras.a)".into(),
                12 => "vec3(1.0)".into(),
                13 => "vec3(0.5)".into(),
                14 => "kc".into(),
                _ => "vec3(0.0)".into(),
            }
        };
        let ain = |sel: u8| -> &'static str {
            match sel {
                0 => "r0.a",
                1 => "r1.a",
                2 => "r2.a",
                3 => "r3.a",
                4 => "tex.a",
                5 => "ras.a",
                6 => "ka",
                _ => "0.0",
            }
        };
        let expr = |e: &StageEnv, a: String, b: String, c: String, d: String, zero: &str| -> String {
            let bias = match e.bias {
                1 => " + 0.5",
                2 => " - 0.5",
                _ => "",
            };
            let scale = ["1.0", "2.0", "4.0", "0.5"][e.scale as usize];
            let op = if e.sub { "-" } else { "+" };
            let body = format!("(({d}) {op} mix({a}, {b}, {c}){bias}) * {scale}");
            if e.clamp {
                format!("clamp({body}, {zero}, {})", zero.replace("0.0", "1.0"))
            } else {
                format!("clamp({body}, {}, {})", zero.replace("0.0", "-4.0"), zero.replace("0.0", "4.0"))
            }
        };
        let e = &st.color;
        let cexpr = expr(e, cin(e.a), cin(e.b), cin(e.c), cin(e.d), "vec3(0.0)");
        let ea = &st.alpha;
        let aexpr = expr(ea, ain(ea.a).into(), ain(ea.b).into(), ain(ea.c).into(), ain(ea.d).into(), "0.0");
        let _ = writeln!(s, "        vec3 c_out = {cexpr};\n        float a_out = {aexpr};");
        let _ = writeln!(s, "        {}.rgb = c_out;\n        {}.a = a_out;\n    }}", reg[e.dest as usize], reg[ea.dest as usize]);
    }
    s.push_str("    vec4 result = clamp(r0, vec4(0.0), vec4(1.0));\n");
    let at = &cfg.alpha_test;
    if !at.always_passes() {
        let cmp = |c: AlphaComp, r: &str| match c {
            AlphaComp::Never => "false".to_string(),
            AlphaComp::Always => "true".to_string(),
            _ => format!("(a8 {} {r})", c.glsl()),
        };
        let x = cmp(at.comp0, "tev.alpha_ref.x");
        let y = cmp(at.comp1, "tev.alpha_ref.y");
        let logic = match at.logic {
            0 => format!("{x} && {y}"),
            1 => format!("{x} || {y}"),
            2 => format!("{x} != {y}"),
            _ => format!("{x} == {y}"),
        };
        let _ = writeln!(s, "    float a8 = floor(result.a * 255.0 + 0.5);\n    if (!({logic})) {{ discard; }}");
    }
    s.push_str("    o_color = result;\n}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_returns_vertex_color() {
        let cfg = TevConfig::passthrough();
        let inp = TevInputs { ras: [[0.25, 0.5, 0.75, 1.0], [0.0; 4]], ..Default::default() };
        let (c, pass) = evaluate(&cfg, &inp);
        assert_eq!(c, [0.25, 0.5, 0.75, 1.0]);
        assert!(pass);
        assert!(cfg.alpha_test.always_passes());
    }

    #[test]
    fn modulate_texture_with_vertex_color() {
        // d = 0, a = 0, b = TEXC, c = RASC → tex * ras
        let color = StageEnv { a: 15, b: 8, c: 10, d: 15, ..Default::default() };
        let alpha = StageEnv { a: 7, b: 4, c: 5, d: 7, ..Default::default() };
        let cfg = TevConfig {
            stages: vec![Stage { color, alpha, tex_enabled: true, ..Default::default() }],
            alpha_test: AlphaTest::from_reg(7 << 16 | 7 << 19),
            num_texgens: 1,
        };
        let mut inp = TevInputs::default();
        inp.ras[0] = [0.5, 1.0, 0.0, 1.0];
        inp.tex[0] = [1.0, 0.5, 1.0, 0.5];
        let (c, _) = evaluate(&cfg, &inp);
        assert_eq!(c, [0.5, 0.5, 0.0, 0.5]);
    }

    #[test]
    fn alpha_test_analysis() {
        let always = AlphaTest::from_reg(7 << 16 | 7 << 19); // ALWAYS AND ALWAYS
        assert!(always.always_passes());
        let or_always = AlphaTest::from_reg(4 << 16 | 7 << 19 | 1 << 22); // GREATER OR ALWAYS
        assert!(or_always.always_passes());
        let cutout = AlphaTest::from_reg(128 | 6 << 16 | 7 << 19); // GEQUAL 128 AND ALWAYS
        assert!(!cutout.always_passes());
        assert!(cutout.passes(200) && !cutout.passes(10));
        let xnor_never = AlphaTest::from_reg(3 << 22); // NEVER XNOR NEVER
        assert!(xnor_never.always_passes());
    }

    #[test]
    fn from_bp_decodes_stage_and_order() {
        let mut bp = [0u32; 256];
        bp[0x00] = 1 << 10 | 1; // 2 stages, 1 texgen
        bp[0x28] = 1 << 6 | 3 | (1 << 3) | (5 << 12) | (1 << 19); // stage0: tex map 3 coord 1 enabled; stage1 channel 1
        bp[0xC0] = 0xF8AF; // d=F, c=A, b=8, a=F → tex * ras
        let cfg = TevConfig::from_bp(&bp);
        assert_eq!(cfg.stages.len(), 2);
        assert_eq!(cfg.stages[0].texmap, 3);
        assert_eq!(cfg.stages[0].texcoord, 1);
        assert!(cfg.stages[0].tex_enabled);
        assert_eq!(cfg.stages[0].color.b, 8);
        assert_eq!(cfg.stages[1].channel, 1);
    }
}
