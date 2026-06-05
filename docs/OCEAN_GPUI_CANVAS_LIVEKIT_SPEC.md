# Ocean GPUI Canvas + LiveKit Spec

Status: active GPUI product direction.

This is the collaboration-surface spec for `crates/ocean-gui`. It is not the
old TUI room system, not a Papyrus note app, and not a wrapper around the web
chat. It is the native desktop space where humans and agents share a live
canvas, voice/video presence, and one Ocean session context.

## Product Shape

Ocean GUI is a desktop collaboration cockpit for remote work:

- humans join a shared working space,
- coworkers can optionally publish mic/camera,
- agents participate through Ocean daemon/Longhouse actions,
- the shared canvas persists visible working memory,
- turns can render cards, workflows, storyboards, proposals, maps, and other
  structured objects onto the canvas.

The chat transcript is table stakes. The canvas is the core product surface.

## Three Planes

Do not collapse these into one transport.

### 1. Canvas Plane

Use tldraw for the multiplayer canvas.

- tldraw document state is its own CRDT/sync domain.
- Humans manipulate the canvas directly.
- Agents render through structured canvas commands that become tldraw shapes.
- The canvas state is not stored as daemon-global chat state.

Initial host path:

```text
GPUI native app
  -> wry webview region
  -> crates/ocean-gui/canvas-web
  -> tldraw editor/runtime
```

The wry pane must have a dedicated non-overlapped region. GPUI chrome should
not rely on floating over the webview because native webviews render as child
layers above GPU UI.

### 2. Transport + Presence Plane

Use LiveKit for realtime presence:

- audio,
- video,
- participant attributes,
- room metadata,
- RPC/data messages.

LiveKit's word "room" means the media/session container. It is not the old TUI
room concept. In product language, prefer "collaboration space", "hangout", or
"surface session" unless the code is specifically naming a LiveKit room.

The GPUI app should use the LiveKit Rust client SDK as a participant. The
Agents framework can remain a later Python/Node sidecar only if needed for
turn detection; the runtime/reasoning authority remains Ocean.

### 3. Reasoning Plane

Ocean daemon remains the authority:

```text
GPUI / web / extension / TUI
  -> ocean-daemon
  -> ocean-agent / ocean-runtime / providers / tools / Longhouse
```

Agents do not own the canvas transport. They receive surface context and emit
structured intent. The app applies that intent to the canvas ledger/tldraw doc.

## Session Model

Use the ecosystem contract:

```text
Project -> Workspace -> Session -> Turns -> Events
Surface -> Session
```

- A session is the reasoning root and transcript/event stream.
- A surface is a UI attached to a session.
- A canvas is a view/document attached to a session.
- A pane is UI layout only.

Do not bind agent memory to a pane. Multiple panes can show different canvases
or transcript views for the same session. Closing a pane must not kill the
session.

First-party surfaces must:

```text
POST /v1/agent/sessions
GET  /v1/agent/events?session_id=<id>
POST /v1/agent/turns { session_id, prompt, cwd, project_id?, client_type: "surface-gpui" }
```

The app may open the same session across GPUI, web, and extension by explicitly
attaching each surface to the same `session_id`. Different sessions must never
bleed events into each other.

## Canvas Ledger

The canvas needs a ledger so humans and agents share spatial memory.

Each visible component should have a durable record:

```json
{
  "id": "brief-1",
  "component_type": "brief_card",
  "x": 450,
  "y": 120,
  "width": 320,
  "height": 220,
  "content": {},
  "metadata": {},
  "connections": []
}
```

Ledger responsibilities:

- record what exists on the canvas,
- expose positions and sizes to the next turn,
- prevent agents from stomping existing work,
- support mode-specific layouts such as workflow builder, storyboard, campaign
  board, proposal review, or map planning,
- keep the canvas as persistent working memory.

The ledger should ride with the canvas/document state, not become a daemon
table. The GPUI app injects the relevant ledger summary into turn context.

## Agent Render Loop

Target loop:

```text
human speaks/types/clicks
  -> GPUI builds session + surface + canvas context
  -> POST /v1/agent/turns
  -> daemon streams AgentTurnEvents
  -> agent emits render commands / component events
  -> GPUI applies commands to ledger + tldraw
  -> all canvas participants converge through tldraw sync
```

The agent should not emit arbitrary web code. It should emit trusted structured
commands such as:

```json
{
  "type": "canvas.render_component",
  "canvas_id": "main",
  "component_type": "proposal_card",
  "placement": { "strategy": "next_available", "near": "brief-1" },
  "content": {}
}
```

The app owns final rendering.

## Write Paths

Two acceptable write paths:

1. GPUI-to-webview IPC: Rust receives daemon events, sends structured commands
   into the tldraw webview, and JS calls tldraw editor APIs.
2. Headless tldraw writer sidecar: a small TypeScript process connects to the
   tldraw sync room and applies render commands directly.

Start with path 1 because it is simpler for the local GPUI prototype. Move to a
headless writer if agent rendering should continue when the visible GPUI pane
is detached or slow.

## Layout / Multiplexing

The layout goal is tmux-like UI plumbing:

- attach/detach canvases,
- split panes,
- show multiple canvases for one session,
- pop a canvas into another window later,
- keep the session as the root, not the pane.

Pane layout is orthogonal to the reasoning loop. The turn context should include
the full active surface topology, not just "the pane the user is in."

## Near-Term Slice Order

1. Keep session scoping correct: no global SSE, no session adoption.
2. Stabilize GPUI chrome and transcript rendering.
3. Make the canvas webview load reliably from `crates/ocean-gui/canvas-web`.
4. Wire GPUI <-> canvas-web IPC for ledger load/update events.
5. Add a minimal tldraw shape render command.
6. Feed ledger summary into `surface-gpui` turn context.
7. Add LiveKit join/presence controls with mic/camera toggles.
8. Use LiveKit metadata/attributes for compact surface/session presence.
9. Add real collaboration auth/token flow through the daemon/proxy.

## Hard Boundaries

- Do not implement this from old TUI room docs.
- Do not introduce "pods" or separate agent mesh concepts in the GUI.
- Do not store canvas CRDT state in LiveKit.
- Do not make the daemon the canvas renderer.
- Do not rely on GPUI overlays above the webview.
- Do not let global SSE pick the active session for a surface.
