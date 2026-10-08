# Pinned egui-wgpu HDR dependency patch

Base: crates.io egui-wgpu 0.36.2, MIT OR Apache-2.0. The source and licenses remain in the dependency fork, not in the PhotoCraft workspace.

Source: https://github.com/TheSeanLavery/egui/tree/24fd06138d67cda864f81824ec05282c2f8e4188

Only egui-wgpu is patched; egui and epaint keep their crates.io 0.36.2 identities to avoid duplicate incompatible UI types.

Local changes expose encoded extended-sRGB Rgba16Float negotiation, shared live display information, and an optional float framebuffer capture callback. Default configuration stays SDR. Float screenshot readback uses eight bytes per pixel and still supplies a clipped SDR ColorImage for existing screenshot callers.

The encoded output space intentionally matches egui's gamma-framebuffer shader, keeping UI white at 1.0 without replacing the renderer. No native/unsafe bridge was added. Replace this dependency patch with an upstream safe configuration hook when available. Do not modify unrelated renderer/input code here.
