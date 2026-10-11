# Room panel and invitations

Click the people icon beside Discord to open the persistent Room panel. The
diagonal-arrow button switches between full and minimal modes. The chevrons
collapse the panel completely; the people icon restores it. Window >
Collaboration is another way to reopen the panel.
Opening an invitation always reveals the full Room dock, overriding a hidden,
minimal or collapsed mode, including when already connected to that room.

Create a room, then use **Copy invite**. Guests can paste a room code or invitation
into Join. Password, connection server and TURN settings live under Advanced.
Profile settings contain the name, cursor color, icon and visibility controls.
Minimal mode keeps the invitation, participants and desktop voice controls.

The production invitation service is
`https://photocraft-mp.theseanlavery.workers.dev`. An invitation such as
`https://photocraft-mp.theseanlavery.workers.dev/join/ABCD1234` redirects to the
Rust/WASM editor at `/app/?room=ABCD1234` and starts joining that room. A password
is requested separately and never included in the URL. The editor host must
remain open to supply the document and order edits; the signaling service runs
independently in Cloudflare Durable Objects.
The browser address updates to the admitted room when creating or joining one,
and removes the room parameter when leaving. Sharing the page therefore uses
the current room rather than an earlier invitation. Joining loads the host's
actual document and layers after the WebRTC checkpoint arrives; it does not
create a replacement blank document.

Installed desktop packages register `photocraft://join/ABCD1234`. The browser's
**Open in PhotoCraft** button uses this protocol. macOS handles both cold launches
and invitations delivered to an existing process. Linux and Windows protocol
handlers launch a process; existing-instance forwarding is not implemented.
Opening an invitation preserves existing documents and refuses to replace an
active different room until the user leaves it. Reopening the same room is
idempotent.

Desktop voice supports holding the push-to-talk button. Browser voice is not
implemented and the panel explicitly offers opening the desktop app. Drawing,
author-scoped undo, chat and room presence work between browser and desktop.

## Deployment

The edge Worker retains the deployed Durable Objects/R2 implementation imported
from commit `2109ba5b`. Preview web assets use the separate `room-panel/` R2
namespace, preserving the existing root frontend. From the repository root,
build with `NO_COLOR=true trunk build --release` in `apps/photocraft-web`, then
run `python3 deploy/cloudflare-edge/publish-web.py --prefix room-panel`.
Publish hashed assets before the index, then deploy the edge Worker from
`deploy/cloudflare-edge`. No separate invite website or desktop tunnel is needed.

## Validation (2026-10-10)

- Native macOS cold and warm protocol opens joined the public service.
- Two native clients performed 16 overlapping brush/eraser strokes with 1,024
  pressure samples and author undo/redo; their decoded RGBA exports matched.
- The deployed HTTPS invitation joined a third browser client. A browser stroke
  changed the native host's export; browser Undo restored the original RGBA hash
  exactly, preserving the native authors' strokes.
- Full, minimal and fully collapsed modes were inspected in native and browser
  runtimes. The room remained connected through all modes.
- The deployed HTML and WASM hashes matched the release build.

Local screenshots and sanitized runtime proof are retained under
`plan/evidence/room-panel/` (gitignored). These checks cover this machine and the
public signaling service; they do not establish every device or network route.
