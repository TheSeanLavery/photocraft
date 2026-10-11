//! Controlled 24 MP comparison of the former bounded Levels LUT and float Levels evaluation.
use photocraft_color::SampleType;
use photocraft_compose::{Buffer, adjust};
use photocraft_doc::Adjustment;
use photocraft_geom::Rect;
use std::time::Instant;
fn main() {
    for gamma in [1.0, 1.3] {
        let adj = Adjustment::Levels {
            master: photocraft_doc::adjust::LevelsChannel { gamma, ..Default::default() },
            per_channel: Default::default(),
            space: Default::default(),
            black: Default::default(),
        };
        let luts = adjust::tone_luts_depth(&adj, Some(SampleType::F32));
        for fixed in [false, true] {
            let mut times = Vec::new();
            let mut peak = 0.0f32;
            for _ in 0..5 {
                let mut buf = Buffer {
                    rect: Rect::new(0, 0, 6000, 4000),
                    px: (0..24_000_000)
                        .map(|i| {
                            let v = (i % 1024) as f32 / 128.0;
                            [v, v * 0.5, v * 0.25, 1.0]
                        })
                        .collect(),
                };
                let start = Instant::now();
                if fixed {
                    adjust::apply_depth(&adj, &mut buf, adjust::Transfer::Gamma(1.0), Some(SampleType::F32));
                } else {
                    for p in &mut buf.px {
                        for (v, table) in p.iter_mut().take(3).zip(&luts) {
                            let x = v.clamp(0.0, 1.0) * 4095.0;
                            let i = x.floor() as usize;
                            let a = table.get(i).copied().unwrap_or(0.0);
                            let b = table.get(i.saturating_add(1).min(4095)).copied().unwrap_or(a);
                            *v = a * (1.0 - x.fract()) + b * x.fract();
                        }
                    }
                }
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                peak = buf.px.iter().map(|p| p[0]).fold(0.0, f32::max);
            }
            times.sort_by(f64::total_cmp);
            println!(
                "24MP gamma={gamma} {} median_ms={:.3} peak={peak:.5}",
                if fixed { "float Levels" } else { "former clipped LUT" },
                times.get(2).copied().unwrap_or(0.0)
            );
        }
    }
}
