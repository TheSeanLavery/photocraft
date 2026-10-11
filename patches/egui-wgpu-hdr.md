# Pinned egui-wgpu HDR dependency patch

Base: crates.io egui-wgpu 0.36.2, MIT OR Apache-2.0. The source and licenses remain in the dependency fork, not in the PhotoCraft workspace.

Source: https://github.com/TheSeanLavery/egui/tree/24fd06138d67cda864f81824ec05282c2f8e4188

Among the egui renderer crates, only egui-wgpu is patched; egui and epaint keep their crates.io 0.36.2 identities to avoid duplicate incompatible UI types.

Local changes expose encoded extended-sRGB Rgba16Float negotiation, shared live display information, and an optional float framebuffer capture callback. Default configuration stays SDR. Float screenshot readback uses eight bytes per pixel and still supplies a clipped SDR ColorImage for existing screenshot callers.

The encoded output space intentionally matches egui's gamma-framebuffer shader, keeping UI white at 1.0 without replacing the renderer. No native/unsafe bridge was added. Replace this dependency patch with an upstream safe configuration hook when available. Do not modify unrelated renderer/input code here.

## Browser presentation

`vendor/eframe` is crates.io eframe 0.36.2 with one browser surface configuration field added: `color_space: render_state.output_color_space`. It uses the existing pinned egui-wgpu HDR negotiation. The web shell enables `surface.hdr` by default; `?sdr`, `?cpu`, and `?webgl` keep SDR. Unsupported float surfaces fall back through the existing capability negotiation. No compositor or color-picker changes are needed.

On HDR-capable browser displays, missing numeric luminance metadata uses a 16× linear preview ceiling; the browser maps this extended signal to the display. This is not a measured physical headroom value. Unknown or SDR displays retain reference-white preview. Native display headroom behavior is unchanged.
