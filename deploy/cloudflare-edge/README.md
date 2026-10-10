# PhotoCraft Cloudflare playtest edge

A Rust Worker provides the stable `https://photocraft-mp.theseanlavery.workers.dev` address. It forwards only the room protocol and dedicated downloads to the HTTPS origin configured in `ROOM_ORIGIN`. Room membership checks and temporary Cloudflare TURN credential generation run in the native Rust room service. The permanent TURN token stays on the origin, outside Git.

This is an independent deployment crate, not an editor dependency. `worker-build` generates the standard wasm-bindgen runtime glue; there is no handwritten JavaScript or TypeScript.

```sh
cargo install worker-build --version 0.8.7 --locked
cargo test
cargo clippy --all-targets -- -D warnings
wrangler deploy --dry-run
wrangler deploy
wrangler secret put ROOM_ORIGIN
```

Paste the public HTTPS tunnel origin into the final command. Changing that secret updates the upstream without requiring everyone to rebuild PhotoCraft. Never point the origin at the native app automation/control listener. Downloads stream without buffering; signaling bodies are limited to 96 KiB. Unknown routes and methods are rejected, and failed origin connections report 503.

The current origin runs on the operator's Mac. This stable edge address does not make the origin always-on. Keep the Mac awake and the native room server and cloudflared running for a playtest. Move the origin to an always-on Rust host later, or adapt room state to a Rust Durable Object.
