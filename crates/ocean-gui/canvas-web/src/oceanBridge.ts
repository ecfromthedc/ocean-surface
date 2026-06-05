import type { Editor } from 'tldraw'
import { createShapeId, toRichText } from 'tldraw'
import type { LedgerComponent, SurfaceIpcCommand, SurfaceIpcEvent } from './ledger'
import { componentText } from './ledger'

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
