import React, { useMemo } from 'react'
import { createRoot } from 'react-dom/client'
import { Tldraw, type Editor, type TLAssetStore } from 'tldraw'
import { useSync } from '@tldraw/sync'
import 'tldraw/tldraw.css'
import './styles.css'
import {
  applyOceanCommand,
  emitOceanEvent,
  installOceanCommandBridge,
  snapshotLedger,
} from './oceanBridge'

interface CanvasConfig {
  sessionId: string
  paneId: string
  canvasId: string
  tldrawRoomId: string
  syncUri?: string
}

function readConfig(): CanvasConfig {
  const params = new URLSearchParams(window.location.search)
  return {
    sessionId: params.get('session_id') || 'surface:main',
    paneId: params.get('pane_id') || 'pane:1',
    canvasId: params.get('canvas_id') || 'canvas:main',
    tldrawRoomId: params.get('tldraw_room_id') || 'ocean-surface-main',
    syncUri: params.get('sync_uri') || undefined,
  }
}

const assetStore: TLAssetStore = {
  async upload(_asset, file) {
    return { src: URL.createObjectURL(file) }
  },
  resolve(asset) {
    return asset.props.src
  },
}

function SyncedCanvas({ config }: { config: CanvasConfig }) {
  const store = useSync({
    uri: `${config.syncUri}/${encodeURIComponent(config.tldrawRoomId)}`,
    assets: assetStore,
  })

  return <Tldraw store={store} onMount={(editor) => installEditor(editor, config)} />
}

function LocalCanvas({ config }: { config: CanvasConfig }) {
  return <Tldraw onMount={(editor) => installEditor(editor, config)} />
}

function installEditor(editor: Editor, config: CanvasConfig) {
  let revision = 0
  const bridgeContext = {
    paneId: config.paneId,
    canvasId: config.canvasId,
    tldrawRoomId: config.tldrawRoomId,
  }

  installOceanCommandBridge(
    (command) => applyOceanCommand(editor, command, bridgeContext),
    bridgeContext,
  )
  emitOceanEvent({
    type: 'canvas_ready',
    pane_id: config.paneId,
    canvas_id: config.canvasId,
    tldraw_room_id: config.tldrawRoomId,
  })

  editor.store.listen(() => {
    revision += 1
    emitOceanEvent(snapshotLedger(editor, config.canvasId, revision))
  })

  editor.on('change', () => {
    emitOceanEvent({
      type: 'selection_changed',
      canvas_id: config.canvasId,
      selected_ids: editor.getSelectedShapes().map((shape) => String(shape.meta.oceanComponentId || shape.id)),
    })
  })
}

function App() {
  const config = useMemo(readConfig, [])
  return (
    <div
      className="ocean-canvas-root"
      data-session-id={config.sessionId}
      data-pane-id={config.paneId}
      data-canvas-id={config.canvasId}
    >
      {config.syncUri ? <SyncedCanvas config={config} /> : <LocalCanvas config={config} />}
    </div>
  )
}

createRoot(document.getElementById('root')!).render(<App />)
