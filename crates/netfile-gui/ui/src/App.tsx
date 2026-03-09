import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import DeviceList from './components/DeviceList'
import TransferQueue from './components/TransferQueue'
import TransferHistory from './components/TransferHistory'
import ShareBrowser from './components/ShareBrowser'
import Settings from './components/Settings'
import './App.css'

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

function App() {
  const [devices, setDevices] = useState<Device[]>([])
  const [transfers, setTransfers] = useState<Transfer[]>([])
  const [showSettings, setShowSettings] = useState(false)
  const [activePanel, setActivePanel] = useState<'queue' | 'history' | 'share'>('queue')

  const devicesRef = useRef<string>('')
  const transfersRef = useRef<string>('')
  const transfersIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const activeRef = useRef<boolean>(false)

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
      <header className="app-header">
        <h1>NetFile</h1>
        <button className="settings-button" onClick={() => setShowSettings(true)}>
          ⚙️ 设置
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
    </div>
  )
}

export default App
