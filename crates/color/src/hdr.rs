//! Original display-referred HDR → SDR operators. Input/output are straight linear RGB.
//! Natural is a hue-preserving exponential shoulder; Filmic adds a gentle toe. Neither is ACES.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToneMap {
    #[default]
    Natural,
    Filmic,
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HdrWorkbench {
    pub method: ToneMap,
    pub exposure: f32,
    pub shoulder: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub split: bool,
    pub clipping: bool,
}
impl Default for HdrWorkbench {
    fn default() -> Self {
        Self { method: ToneMap::Natural, exposure: 0.0, shoulder: 0.75, contrast: 1.0, saturation: 1.0, split: false, clipping: false }
    }
}
fn finite(v: f32, fallback: f32, lo: f32, hi: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}
impl HdrWorkbench {
    pub fn sanitized(self) -> Self {
        Self {
            exposure: finite(self.exposure, 0.0, -20.0, 20.0),
            shoulder: finite(self.shoulder, 0.75, 0.1, 0.95),
            contrast: finite(self.contrast, 1.0, 0.25, 2.0),
            saturation: finite(self.saturation, 1.0, 0.0, 2.0),
            ..self
        }
    }
    /// Display grading followed by SDR compression, or headroom clipping for HDR.
    /// Peak-channel scaling preserves RGB ratios through the shoulder; alpha is independent.
    pub fn apply(self, rgb: [f32; 3], headroom: f32) -> [f32; 3] {
        let s = self.sanitized();
        let mut c = s.grade(rgb);
        let peak = c[0].max(c[1]).max(c[2]);
        let ceiling = finite(headroom, 1.0, 1.0, 16.0);
        if ceiling <= 1.0 && s.method != ToneMap::Clip && peak > 0.0 {
            let k = s.shoulder;
            let mut mapped = if peak <= k { peak } else { k + (1.0 - k) * (1.0 - (-(peak - k) / (1.0 - k)).exp()) };
            if s.method == ToneMap::Filmic {
                mapped = mapped * mapped / (mapped + 0.04) * 1.04;
            }
            c = c.map(|v| v * mapped / peak);
        }
        c.map(|v| v.clamp(0.0, ceiling))
    }
    fn grade(self, rgb: [f32; 3]) -> [f32; 3] {
        let s = self.sanitized();
        let mut c = rgb.map(|v| finite(v, 0.0, 0.0, 65504.0) * 2.0f32.powf(s.exposure));
        c = c.map(|v| (v / 0.18).max(0.0).powf(s.contrast) * 0.18);
        let l = c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
        c = c.map(|v| (l + (v - l) * s.saturation).max(0.0));
        c
    }
    pub fn clipped(self, rgb: [f32; 3], headroom: f32) -> bool {
        rgb.iter().any(|v| !v.is_finite() || *v < 0.0) || self.grade(rgb).into_iter().any(|v| v > headroom.max(1.0))
    }
}

/// Stops relative to reference white; zero and negative values occupy the leftmost bin.
pub const HISTOGRAM_BINS: usize = 128;
pub fn histogram(pixels: impl IntoIterator<Item = [f32; 4]>) -> [u64; HISTOGRAM_BINS] {
    let mut bins = [0u64; HISTOGRAM_BINS];
    for p in pixels {
        if !p[3].is_finite() || p[3] <= 0.0 {
            continue;
        }
        let peak = p[0].max(p[1]).max(p[2]);
        let stop = if peak.is_finite() && peak > 0.0 { peak.log2().clamp(-8.0, 8.0) } else { -8.0 };
        let index = ((stop + 8.0) * 8.0).floor() as usize;
        if let Some(bin) = bins.get_mut(index.min(HISTOGRAM_BINS - 1)) {
            *bin = bin.saturating_add(1);
        }
    }
    bins
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shoulder_preserves_midtones_hue_and_highlight_order() {
        let s = HdrWorkbench::default();
        assert!(s.apply([0.18; 3], 1.0).into_iter().all(|v| (v - 0.18).abs() < 1e-6));
        let a = s.apply([1.0, 0.5, 0.25], 1.0);
        let b = s.apply([2.0, 1.0, 0.5], 1.0);
        assert!(a[0] < b[0] && b[0] < 1.0);
        assert!((a[1] / a[0] - 0.5).abs() < 1e-6);
        assert_eq!(s.apply([4.0; 3], 4.0), [4.0; 3]);
    }
    #[test]
    fn hostile_values_and_settings_stay_finite() {
        let s = HdrWorkbench { exposure: f32::INFINITY, contrast: f32::NAN, shoulder: 1.0, ..Default::default() };
        for method in [ToneMap::Natural, ToneMap::Filmic, ToneMap::Clip] {
            let s = HdrWorkbench { method, ..s };
            for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, -1.0, 0.0, 4.0] {
                assert!(s.apply([v; 3], 1.0).into_iter().all(|v| v.is_finite() && (0.0..=1.0).contains(&v)));
            }
        }
    }
    #[test]
    fn histogram_white_and_stops() {
        let h = histogram([[1.0, 1.0, 1.0, 1.0], [4.0, 4.0, 4.0, 1.0], [8.0, 8.0, 8.0, 0.0]]);
        assert_eq!(h[64], 1);
        assert_eq!(h[80], 1);
        assert_eq!(h.iter().sum::<u64>(), 2);
    }
}
