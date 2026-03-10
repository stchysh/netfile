import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import DeviceList from './components/DeviceList'
import TransferQueue from './components/TransferQueue'
import TransferHistory from './components/TransferHistory'
import ShareBrowser from './components/ShareBrowser'
import Settings from './components/Settings'
import './App.css'

const APP_VERSION = '0.1.0'

interface Device {
  device_id: string
  instance_id: string
  device_name: string
  instance_name: string
  ip: string
  port: number
  version: string
  is_self: boolean
  public_transfer_addr?: string
  discovery_port?: number
}

interface Transfer {
  file_id: string
  file_name: string
  total_size: number
  transferred: number
  total_chunks: number
  completed_chunks: number
  speed: number
  eta_secs: number
  elapsed_secs: number
  direction: string
  status: string
  paused: boolean
}

interface Toast {
  id: number
  message: string
}

function App() {
  const [devices, setDevices] = useState<Device[]>([])
  const [transfers, setTransfers] = useState<Transfer[]>([])
  const [showSettings, setShowSettings] = useState(false)
  const [activePanel, setActivePanel] = useState<'queue' | 'history' | 'share'>('queue')
  const [newVersion, setNewVersion] = useState<string | null>(null)
  const [toasts, setToasts] = useState<Toast[]>([])
  const toastIdRef = useRef(0)

  const devicesRef = useRef<string>('')
  const transfersRef = useRef<string>('')
  const transfersIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const activeRef = useRef<boolean>(false)
  const prevActiveTransferIds = useRef<Set<string>>(new Set())

  const addToast = (message: string) => {
    const id = ++toastIdRef.current
    setToasts(prev => [...prev, { id, message }])
    setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 4000)
  }

  useEffect(() => {
    fetch('https://api.github.com/repos/stchysh/netfile/releases/latest')
      .then(r => r.json())
      .then(data => {
        const tag: string = data?.tag_name ?? ''
        const remote = tag.replace(/^v/, '')
        if (remote && remote !== APP_VERSION) {
          setNewVersion(remote)
        }
      })
      .catch(() => {})
  }, [])

  useEffect(() => {
    const fetchDevices = async () => {
      try {
        const result = await invoke<Device[]>('get_devices')
        const key = JSON.stringify(result)
        if (key !== devicesRef.current) {
          devicesRef.current = key
          setDevices(result)
        }
      } catch (error) {
        console.error('Failed to fetch devices:', error)
      }
    }

    const scheduleTransfers = (fast: boolean) => {
      if (transfersIntervalRef.current) clearInterval(transfersIntervalRef.current)
      transfersIntervalRef.current = setInterval(fetchTransfers, fast ? 500 : 2000)
    }

    const fetchTransfers = async () => {
      try {
        const result = await invoke<Transfer[]>('get_transfers')
        const key = JSON.stringify(result)
        if (key !== transfersRef.current) {
          transfersRef.current = key
          const currentActiveIds = new Set(
            result.filter(t => t.status !== 'Pending' && t.status !== 'AwaitingConfirmation').map(t => t.file_id)
          )
          for (const id of prevActiveTransferIds.current) {
            if (!currentActiveIds.has(id)) {
              const prev = result.find(t => t.file_id === id)
              if (!prev) {
                addToast('传输完成')
              }
            }
          }
          prevActiveTransferIds.current = currentActiveIds
          setTransfers(result)
        }
        const hasActive = result.length > 0
        if (hasActive !== activeRef.current) {
          activeRef.current = hasActive
          scheduleTransfers(hasActive)
        }
      } catch (error) {
        console.error('Failed to fetch transfers:', error)
      }
    }

    fetchDevices()
    fetchTransfers()

    const devicesInterval = setInterval(fetchDevices, 2000)
    scheduleTransfers(false)

    return () => {
      clearInterval(devicesInterval)
      if (transfersIntervalRef.current) clearInterval(transfersIntervalRef.current)
    }
  }, [])

  return (
    <div className="app">
      {newVersion && (
        <div className="version-banner">
          新版本 v{newVersion} 可用，<a href="https://github.com/stchysh/netfile/releases/latest" target="_blank" rel="noreferrer">前往下载</a>
          <button className="version-banner-close" onClick={() => setNewVersion(null)}>x</button>
        </div>
      )}
      <header className="app-header">
        <h1>NetFile</h1>
        <button className="settings-button" onClick={() => setShowSettings(true)}>
          设置
        </button>
      </header>
      <div className="app-content">
        <DeviceList devices={devices} />
        <div className="right-panel">
          <div className="panel-tabs">
            <button
              className={`panel-tab ${activePanel === 'queue' ? 'panel-tab-active' : ''}`}
              onClick={() => setActivePanel('queue')}
            >
              传输队列
            </button>
            <button
              className={`panel-tab ${activePanel === 'history' ? 'panel-tab-active' : ''}`}
              onClick={() => setActivePanel('history')}
            >
              传输记录
            </button>
            <button
              className={`panel-tab ${activePanel === 'share' ? 'panel-tab-active' : ''}`}
              onClick={() => setActivePanel('share')}
            >
              共享
            </button>
          </div>
          {activePanel === 'queue' && <TransferQueue transfers={transfers} />}
          {activePanel === 'history' && <TransferHistory />}
          {activePanel === 'share' && <ShareBrowser />}
        </div>
      </div>

      {showSettings && <Settings onClose={() => setShowSettings(false)} />}

      <div className="toast-container">
        {toasts.map(t => (
          <div key={t.id} className="toast">{t.message}</div>
        ))}
      </div>
    </div>
  )
}

export default App
