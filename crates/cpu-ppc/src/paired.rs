//! Paired-single helpers: GQR-driven quantised load/store conversion and
//! the element-wise arithmetic, with AArch64 NEON (`float32x2_t`) fast
//! paths. The scalar versions are the reference implementation and are what
//! runs on x86 hosts.

/// GQR load/store data types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuantType {
    Float,
    U8,
    U16,
    S8,
    S16,
}

impl QuantType {
    pub fn from_bits(v: u32) -> Self {
        match v & 7 {
            4 => QuantType::U8,
            5 => QuantType::U16,
            6 => QuantType::S8,
            7 => QuantType::S16,
            _ => QuantType::Float,
        }
    }

    pub fn size(self) -> u32 {
        match self {
            QuantType::Float => 4,
            QuantType::U8 | QuantType::S8 => 1,
            QuantType::U16 | QuantType::S16 => 2,
        }
    }
}

/// Decoded GQR half (load or store side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quant {
    pub ty: QuantType,
    /// Signed 6-bit scale.
    pub scale: i32,
}

impl Quant {
    pub fn load(gqr: u32) -> Self {
        Self { ty: QuantType::from_bits(gqr >> 16), scale: (((gqr >> 24) << 26) as i32) >> 26 }
    }
    pub fn store(gqr: u32) -> Self {
        Self { ty: QuantType::from_bits(gqr), scale: (((gqr >> 8) << 26) as i32) >> 26 }
    }
}

/// Raw big-endian-decoded value → float.
pub fn dequantize(raw: u32, q: Quant) -> f32 {
    let factor = (-q.scale as f32).exp2();
    match q.ty {
        QuantType::Float => f32::from_bits(raw),
        QuantType::U8 => (raw as u8) as f32 * factor,
        QuantType::U16 => (raw as u16) as f32 * factor,
        QuantType::S8 => (raw as u8 as i8) as f32 * factor,
        QuantType::S16 => (raw as u16 as i16) as f32 * factor,
    }
}

/// Float → raw value (truncating, saturating) for a quantised store.
pub fn quantize(v: f32, q: Quant) -> u32 {
    if q.ty == QuantType::Float {
        return v.to_bits();
    }
    let x = v * (q.scale as f32).exp2();
    match q.ty {
        QuantType::U8 => x.clamp(0.0, 255.0) as u8 as u32,
        QuantType::U16 => x.clamp(0.0, 65535.0) as u16 as u32,
        QuantType::S8 => x.clamp(-128.0, 127.0) as i8 as u8 as u32,
        QuantType::S16 => x.clamp(-32768.0, 32767.0) as i16 as u16 as u32,
        QuantType::Float => unreachable!(),
    }
}

#[inline]
pub fn round_single(x: f64) -> f64 {
    (x as f32) as f64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Element-wise paired-single arithmetic, results rounded to single.
#[inline]
pub fn ps_arith(op: PsOp, a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is mandatory on AArch64.
        unsafe { neon::arith(op, a, b) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    ps_arith_scalar(op, a, b)
}

pub fn ps_arith_scalar(op: PsOp, a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let f = |x: f64, y: f64| {
        round_single(match op {
            PsOp::Add => x + y,
            PsOp::Sub => x - y,
            PsOp::Mul => x * y,
            PsOp::Div => x / y,
        })
    };
    [f(a[0], b[0]), f(a[1], b[1])]
}

/// Dequantise two values at once (psq_l with W = 0).
#[inline]
pub fn dequantize_pair(raw: [u32; 2], q: Quant) -> [f64; 2] {
    #[cfg(target_arch = "aarch64")]
    if q.ty != QuantType::Float {
        // SAFETY: NEON is mandatory on AArch64.
        return unsafe { neon::dequantize_pair(raw, q) };
    }
    [dequantize(raw[0], q) as f64, dequantize(raw[1], q) as f64]
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::*;
    use core::arch::aarch64::*;

    #[inline]
    unsafe fn load(a: [f64; 2]) -> float32x2_t {
        vcvt_f32_f64(vld1q_f64(a.as_ptr()))
    }

    #[inline]
    unsafe fn store(v: float32x2_t) -> [f64; 2] {
        let mut out = [0f64; 2];
        vst1q_f64(out.as_mut_ptr(), vcvt_f64_f32(v));
        out
    }

    pub unsafe fn arith(op: PsOp, a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
        // Inputs that are not exactly representable in single precision
        // must go through the double-precision reference path.
        if round_single(a[0]) != a[0]
            || round_single(a[1]) != a[1]
            || round_single(b[0]) != b[0]
            || round_single(b[1]) != b[1]
        {
            return ps_arith_scalar(op, a, b);
        }
        let (va, vb) = (load(a), load(b));
        let r = match op {
            PsOp::Add => vadd_f32(va, vb),
            PsOp::Sub => vsub_f32(va, vb),
            PsOp::Mul => vmul_f32(va, vb),
            PsOp::Div => vdiv_f32(va, vb),
        };
        store(r)
    }

    pub unsafe fn dequantize_pair(raw: [u32; 2], q: Quant) -> [f64; 2] {
        let ints: [i32; 2] = match q.ty {
            QuantType::U8 => [raw[0] as u8 as i32, raw[1] as u8 as i32],
            QuantType::U16 => [raw[0] as u16 as i32, raw[1] as u16 as i32],
            QuantType::S8 => [raw[0] as u8 as i8 as i32, raw[1] as u8 as i8 as i32],
            QuantType::S16 => [raw[0] as u16 as i16 as i32, raw[1] as u16 as i16 as i32],
            QuantType::Float => return [f32::from_bits(raw[0]) as f64, f32::from_bits(raw[1]) as f64],
        };
        let v = vcvt_f32_s32(vld1_s32(ints.as_ptr()));
        let scaled = vmul_n_f32(v, (-q.scale as f32).exp2());
        store(scaled)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn neon_matches_scalar() {
            let a = [1.5, -2.25];
            let b = [0.125, 3.0];
            for op in [PsOp::Add, PsOp::Sub, PsOp::Mul, PsOp::Div] {
                unsafe { assert_eq!(arith(op, a, b), ps_arith_scalar(op, a, b)) };
            }
            let q = Quant { ty: QuantType::S16, scale: 8 };
            let raw = [0xFF00u32, 0x0180];
            unsafe {
                assert_eq!(dequantize_pair(raw, q), [dequantize(raw[0], q) as f64, dequantize(raw[1], q) as f64]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gqr_decoding() {
        // LD_TYPE s16, LD_SCALE 8; ST_TYPE u8, ST_SCALE -1 (0x3F)
        let gqr = (8 << 24) | (7 << 16) | (0x3F << 8) | 4;
        assert_eq!(Quant::load(gqr), Quant { ty: QuantType::S16, scale: 8 });
        assert_eq!(Quant::store(gqr), Quant { ty: QuantType::U8, scale: -1 });
    }

    #[test]
    fn quantize_round_trip_and_saturation() {
        let q = Quant { ty: QuantType::S16, scale: 8 };
        assert_eq!(dequantize(0x0180, q), 1.5);
        assert_eq!(dequantize(0xFF00, q), -1.0);
        assert_eq!(quantize(1.5, q), 0x0180);
        assert_eq!(quantize(1000.0, q), 0x7FFF, "saturates");
        let u8q = Quant { ty: QuantType::U8, scale: 0 };
        assert_eq!(quantize(-3.0, u8q), 0);
        assert_eq!(quantize(300.0, u8q), 255);
        assert_eq!(quantize(2.9, u8q), 2, "truncates");
        let fq = Quant { ty: QuantType::Float, scale: 5 };
        assert_eq!(dequantize(1.25f32.to_bits(), fq), 1.25, "floats ignore scale");
    }

    #[test]
    fn pair_ops() {
        assert_eq!(ps_arith(PsOp::Add, [1.0, 2.0], [0.5, 0.25]), [1.5, 2.25]);
        assert_eq!(ps_arith(PsOp::Div, [1.0, 1.0], [3.0, 4.0]), [round_single(1.0 / 3.0), 0.25]);
        let q = Quant { ty: QuantType::U8, scale: 1 };
        assert_eq!(dequantize_pair([4, 255], q), [2.0, 127.5]);
    }
}
