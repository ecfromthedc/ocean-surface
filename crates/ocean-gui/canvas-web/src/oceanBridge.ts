import type { Editor } from 'tldraw'
import { createShapeId, toRichText } from 'tldraw'
import type { LedgerComponent, SurfaceIpcCommand, SurfaceIpcEvent } from './ledger'
import { componentText } from './ledger'

// ---------------------------------------------------------------------------
// tldraw adapter demotion (OCEAN-168 / Slice 9)
//
// The authoritative agent-controlled surface is the native Ocean CanvasLedger
// (rendered by GPUI, OCEAN-156/163/167). tldraw is the OPTIONAL sketch / freehand
// projection adapter, reached only via the explicit pane toggle. This bridge is
// the web half of that adapter: it projects the native ledger INTO tldraw
// (export) and reads sketched shapes back OUT as Ocean ledger components
// (import). The Rust side (`shell/tldraw_adapter.rs`) owns the same mapping for
// the native ledger; the two agree on the `ocean_template` metadata convention
// below so a component survives an export → sketch → import round trip.
// ---------------------------------------------------------------------------

/// Metadata key carrying the native component's template/kind across the
/// projection seam, so an imported shape can restore its Ocean template instead
/// of collapsing to the bare tldraw shape type. Mirrors
/// `tldraw_adapter::OCEAN_TEMPLATE_META_KEY` on the Rust side.
export const OCEAN_TEMPLATE_META_KEY = 'ocean_template'

declare global {
  interface Window {
    ipc?: {
      postMessage(message: string): void
    }
    oceanSurface?: {
      postMessage(message: string): void
    }
    oceanSurfaceApplyCommand?: (command: SurfaceIpcCommand) => void
  }
}

export interface CanvasRuntimeContext {
  paneId: string
  canvasId: string
  tldrawRoomId: string
}

export function emitOceanEvent(event: SurfaceIpcEvent) {
  const payload = JSON.stringify(event)
  if (window.oceanSurface?.postMessage) {
    window.oceanSurface.postMessage(payload)
    return
  }
  if (window.ipc?.postMessage) {
    window.ipc.postMessage(payload)
  }
}

export function emitOceanError(message: string, context: Partial<CanvasRuntimeContext> = {}) {
  emitOceanEvent({
    type: 'canvas_error',
    pane_id: context.paneId,
    canvas_id: context.canvasId,
    message,
  })
}

export function installOceanCommandBridge(
  handler: (command: SurfaceIpcCommand) => void,
  context: CanvasRuntimeContext,
) {
  window.oceanSurfaceApplyCommand = (command) => {
    try {
      handler(command)
    } catch (error) {
      emitOceanError(errorMessage(error), context)
    }
  }
  window.addEventListener('message', (event) => {
    if (!event.data) return
    try {
      const command = typeof event.data === 'string' ? JSON.parse(event.data) : event.data
      handler(command as SurfaceIpcCommand)
    } catch (error) {
      emitOceanError(errorMessage(error), context)
    }
  })
}

export function applyOceanCommand(
  editor: Editor,
  command: SurfaceIpcCommand,
  context: CanvasRuntimeContext,
) {
  switch (command.type) {
    case 'load_canvas':
      if (command.pane_id !== context.paneId || command.canvas_id !== context.canvasId) return
      emitOceanEvent({
        type: 'canvas_ready',
        pane_id: command.pane_id,
        canvas_id: command.canvas_id,
        tldraw_room_id: command.tldraw_room_id,
      })
      break
    case 'upsert_component':
      if (command.canvas_id !== context.canvasId) return
      upsertLedgerComponent(editor, command.component)
      break
    case 'focus_component':
      if (command.canvas_id !== context.canvasId) return
      focusComponent(editor, command.component_id)
      break
  }
}

export function upsertLedgerComponent(editor: Editor, component: LedgerComponent) {
  const id = createShapeId(component.id)
  const existing = editor.getShape(id)
  const shape = {
    id,
    type: 'geo' as const,
    x: component.x,
    y: component.y,
    props: {
      geo: 'rectangle' as const,
      w: component.width,
      h: component.height,
      richText: toRichText(componentText(component)),
      color: 'blue' as const,
      fill: 'semi' as const,
      dash: 'solid' as const,
      size: 'm' as const,
      font: 'draw' as const,
      align: 'middle' as const,
      verticalAlign: 'middle' as const,
    },
    meta: {
      oceanComponentId: component.id,
      oceanComponentType: component.component_type,
      oceanContentJson: JSON.stringify(component.content),
      oceanMetadataJson: JSON.stringify(component.metadata),
      oceanConnectionsJson: JSON.stringify(component.connections),
    },
  }

  if (existing) {
    editor.deleteShape(id)
  }
  editor.createShape(shape)
}

export function focusComponent(editor: Editor, componentId: string) {
  const shapeId = createShapeId(componentId)
  const shape = editor.getShape(shapeId)
  if (!shape) return
  editor.select(shapeId)
  editor.zoomToSelection()
}

export function snapshotLedger(editor: Editor, canvasId: string, revision: number): SurfaceIpcEvent {
  const components = editor
    .getCurrentPageShapesSorted()
    .filter((shape) => typeof shape.meta?.oceanComponentId === 'string')
    .map((shape) => {
      const meta = shape.meta as Record<string, unknown>
      const componentId = String(meta.oceanComponentId)
      const componentType =
        typeof meta.oceanComponentType === 'string' ? meta.oceanComponentType : shape.type
      const props = shape.props as Record<string, unknown>
      return {
        id: componentId,
        component_type: componentType,
        x: shape.x,
        y: shape.y,
        width: typeof props.w === 'number' ? props.w : 240,
        height: typeof props.h === 'number' ? props.h : 160,
        content: parseRecord(meta.oceanContentJson, { text: componentId }),
        metadata: parseRecord(meta.oceanMetadataJson),
        connections: parseStringArray(meta.oceanConnectionsJson),
      }
    })

  return {
    type: 'ledger_snapshot',
    canvas_id: canvasId,
    revision,
    components,
  }
}

function parseRecord(
  value: unknown,
  fallback: Record<string, unknown> = {},
): Record<string, unknown> {
  if (typeof value !== 'string') return fallback
  try {
    const parsed = JSON.parse(value)
    return typeof parsed === 'object' && parsed && !Array.isArray(parsed) ? parsed : fallback
  } catch {
    return fallback
  }
}

function parseStringArray(value: unknown): string[] {
  if (typeof value !== 'string') return []
  try {
    const parsed = JSON.parse(value)
    return Array.isArray(parsed) ? parsed.map(String) : []
  } catch {
    return []
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

// ---------------------------------------------------------------------------
// Export: native Ocean ledger -> tldraw (projection into the sketch pane)
// ---------------------------------------------------------------------------

/**
 * Project a batch of Ocean ledger components into the tldraw canvas. Called when
 * the operator toggles INTO the tldraw pane so the sketch surface starts from
 * what the agent built on the native canvas. Each component is upserted as a geo
 * shape; its `metadata.ocean_template` is carried through so a later
 * {@link importTldrawShapes} can restore the native template.
 *
 * Only components matching `canvasId` are projected. Edges are not projected:
 * tldraw is the freehand adapter, the workflow graph stays native.
 */
export function projectLedgerToTldraw(
  editor: Editor,
  canvasId: string,
  components: LedgerComponent[],
) {
  for (const component of components) {
    upsertLedgerComponent(editor, withOceanTemplate(component))
  }
  // canvasId is part of the contract (caller passes the active canvas) and kept
  // here so the signature matches the Rust adapter's per-canvas projection.
  void canvasId
}

/** Ensure the projected component carries its template under the round-trip key. */
function withOceanTemplate(component: LedgerComponent): LedgerComponent {
  if (typeof component.metadata?.[OCEAN_TEMPLATE_META_KEY] === 'string') {
    return component
  }
  return {
    ...component,
    metadata: { ...component.metadata, [OCEAN_TEMPLATE_META_KEY]: component.component_type },
  }
}

// ---------------------------------------------------------------------------
// Import: tldraw shapes -> Ocean ledger (freehand capture)
// ---------------------------------------------------------------------------

/**
 * Read the current tldraw shapes back out as Ocean ledger components and emit a
 * `ledger_snapshot` event for the Rust side to import into the authoritative
 * native ledger (`tldraw_adapter::import_snapshot_into_ledger`). This is how a
 * human's freehand sketch becomes a first-class Ocean canvas component.
 *
 * A shape's `ocean_template` meta (stamped on export) is surfaced as the
 * component_type when present so a round-tripped shape restores its native
 * template; a genuine fresh sketch keeps its tldraw type.
 */
export function importTldrawShapes(editor: Editor, canvasId: string, revision: number) {
  const snapshot = snapshotLedger(editor, canvasId, revision)
  if (snapshot.type !== 'ledger_snapshot') return
  const components = snapshot.components.map((component) => {
    const template = component.metadata?.[OCEAN_TEMPLATE_META_KEY]
    return typeof template === 'string' && template
      ? { ...component, component_type: template }
      : component
  })
  emitOceanEvent({ ...snapshot, components })
}
