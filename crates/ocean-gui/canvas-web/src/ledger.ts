export type SurfaceMode = 'general' | 'workflow_builder' | 'storyboard' | 'campaign_board'

export interface LedgerComponent {
  id: string
  component_type: string
  x: number
  y: number
  width: number
  height: number
  content: Record<string, unknown>
  metadata: Record<string, unknown>
  connections: string[]
}

export interface SurfacePaneContext {
  pane_id: string
  title: string
  kind: 'tldraw_canvas' | 'agent_transcript' | 'notes'
  canvas_id?: string
  dock: 'full' | 'left' | 'right' | 'detached'
}

export interface SurfaceCanvasContext {
  canvas_id: string
  tldraw_room_id: string
  mode: SurfaceMode
  revision: number
  components: LedgerComponent[]
  selection: string[]
  metadata: Record<string, unknown>
}

export interface SurfaceTurnContext {
  session_id: string
  active_pane_id: string
  panes: SurfacePaneContext[]
  canvases: SurfaceCanvasContext[]
}

export type SurfaceIpcEvent =
  | {
      type: 'canvas_ready'
      pane_id: string
      canvas_id: string
      tldraw_room_id: string
    }
  | {
      type: 'ledger_snapshot'
      canvas_id: string
      revision: number
      components: LedgerComponent[]
    }
  | {
      type: 'selection_changed'
      canvas_id: string
      selected_ids: string[]
    }
  | {
      type: 'canvas_error'
      pane_id?: string
      canvas_id?: string
      message: string
    }

export type SurfaceIpcCommand =
  | {
      type: 'load_canvas'
      pane_id: string
      canvas_id: string
      tldraw_room_id: string
    }
  | {
      type: 'upsert_component'
      canvas_id: string
      component: LedgerComponent
    }
  | {
      type: 'focus_component'
      canvas_id: string
      component_id: string
    }

export function componentText(component: LedgerComponent): string {
  const value = component.content.text
  return typeof value === 'string' && value.trim() ? value : component.component_type
}
