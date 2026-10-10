# PhotoCraft room signaling

Run `cargo run -p photocraft-room-server`. The default address is
`127.0.0.1:5548` (registered in Documents/ports.txt). Set
`PHOTOCRAFT_ROOM_BIND=0.0.0.0:5548` for LAN access. Internet deployment should
terminate HTTPS at a reverse proxy and apply request rate limiting. Room data
is memory-only; restarting the server closes room admission.

`POST /create` and `/join` take `{ "code": null, "password": null }` (join
requires a code). The response contains a random room code, peer identifier,
private bearer token and host identifier. Do not share the bearer token.
`POST /signal` routes an SDP offer/answer between an admitted member and host;
`POST /poll` drains only that member's bounded SDP queue. Both require the
`code`, `peer`, and `token` admission credentials. `/leave` closes the member,
or the whole room if the host leaves. Room expiry is one hour without any
admitted member polling; a host missing its heartbeat for two minutes closes
admission. Stale members are reclaimed on joining after two minutes. Room capacity is 64 members, with 256 rooms per server.

Drawing, cursors and audio never pass through this service. Native clients use
DTLS-encrypted WebRTC SCTP data channels directly to the host. Configure STUN
and TURN through `RTCIceServer` for NAT traversal; localhost tests use host
candidates. Three independent channels carry reliable ordered edit operations,
unordered zero-retransmission presence, and unordered zero-retransmission
voice frames. Each packet is capped at 48 KiB and send buffering at 1 MiB;
the shared wire layer encodes reliable messages as bounded CBOR with byte-string resources and streams 16 KiB binary fragments. Transfers and aggregate reassembly are capped at 512 MiB. Native fanout shares encoded buffers across recipients; per-peer bounded edit writers run independently from presence/voice writers. Late join checkpoints send the original baseline once, complete author journal, redo state and unfinished stroke chunks; clients reconstruct the current canonical document. Receivers also accept legacy checkpoints containing a separate current document.

The current admission service is suitable for local testing and protected
small deployments. It has no account identity, host migration, persistent
room storage, endpoint-level attempt limits or TURN provisioning. Room codes
are locators; an optional password protects admission. Passwords are never
stored as plaintext, but the in-memory salted digest is not a password KDF.

The native GUI bridge is `photocraft_collab::transport::Worker`. `requests`
supports `try_send` and `events` supports `try_recv`; neither should block the
GUI frame. Send peer=None to broadcast from the host or send to the client's
single host connection. Incoming edits must pass protocol validation and host
ordering before rebroadcasting. Transport errors require showing an actionable
status and resynchronizing before continuing divergent document operations.

Protocol references: [WebRTC](https://www.w3.org/TR/webrtc/) and
[RFC 8831 data channels](https://www.rfc-editor.org/rfc/rfc8831).

## Reproducible network and drawing evidence

`cargo test -p photocraft-collab --features native --test webrtc` exercises
real encrypted localhost packets on all three channels, password/admission
boundaries and the native nonblocking worker bridge. Add `-- --ignored
--nocapture` to run 32 direct connections and a host plus 31 room workers.
The latter stops signaling while established peers continue streaming; a
500 ms gate checks that HTTP failures cannot block live drawing.

`cargo run -p photocraft-room-server --features doodles --bin
collaboration-doodles -- /absolute/output` starts one authoritative native
engine with 31 real WebRTC replicas. All 32 artists submit pressure, tilt,
rotation and seeded brush settings in 12 incremental chunks. It exports PNG
progress frames, verifies identical final pixels on every replica and verifies
that a per-author undo converges. `report.json` records application payload
bytes and stage timing (including serial rendering on all 32 engines).

To join a room hosted by two visible native PhotoCraft windows with 30 extra
artists, run the same binary with `--join http://127.0.0.1:5548 ROOMCODE -
/absolute/output`. Replace the dash with the optional password. Every bot
loads the real document bootstrap, shows a named colored cursor, sends its
doodle in 12 chunks and verifies pixel convergence with the other bots.
Use a 1024 by 512 canvas for the default eight-column doodle layout. The
native windows remain the visual acceptance check; headless PNGs alone do
not prove the UI or live cursor rendering.
