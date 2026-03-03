import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import DeviceList from './components/DeviceList'
import TransferQueue from './components/TransferQueue'
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
  direction: string
}

function App() {
  const [devices, setDevices] = useState<Device[]>([])
  const [transfers, setTransfers] = useState<Transfer[]>([])
  const [showSettings, setShowSettings] = useState(false)
  const [transferError, setTransferError] = useState<string | null>(null)
  const [transferSuccess, setTransferSuccess] = useState<string | null>(null)
  const [transferStatus, setTransferStatus] = useState<string | null>(null)

  useEffect(() => {
    const fetchDevices = async () => {
      try {
        const result = await invoke<Device[]>('get_devices')
        setDevices(result)
      } catch (error) {
        console.error('Failed to fetch devices:', error)
      }
    }

    const fetchTransfers = async () => {
      try {
        const result = await invoke<Transfer[]>('get_transfers')
        setTransfers(result)
      } catch (error) {
        console.error('Failed to fetch transfers:', error)
      }
    }

    fetchDevices()
    fetchTransfers()

    const devicesInterval = setInterval(fetchDevices, 1000)
    const transfersInterval = setInterval(fetchTransfers, 500)

    const unlistenStarted = listen<string>('transfer-started', (event) => {
      setTransferStatus(`正在发送: ${event.payload}`)
      setTransferError(null)
      setTransferSuccess(null)
    })

    const unlistenComplete = listen<string>('transfer-complete', (event) => {
      setTransferStatus(null)
      setTransferSuccess(`发送成功: ${event.payload}`)
    })

    const unlistenError = listen<string>('transfer-error', (event) => {
      setTransferStatus(null)
      setTransferError(event.payload)
    })

    return () => {
      clearInterval(devicesInterval)
      clearInterval(transfersInterval)
      unlistenStarted.then((fn) => fn())
      unlistenComplete.then((fn) => fn())
      unlistenError.then((fn) => fn())
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
      {transferStatus && (
        <div className="transfer-status-banner">
          <span>{transferStatus}</span>
        </div>
      )}
      {transferError && (
        <div className="transfer-error-banner">
          <span>发送失败: {transferError}</span>
          <button onClick={() => setTransferError(null)}>×</button>
        </div>
      )}
      {transferSuccess && (
        <div className="transfer-success-banner">
          <span>{transferSuccess}</span>
          <button onClick={() => setTransferSuccess(null)}>×</button>
        </div>
      )}
      <div className="app-content">
        <DeviceList devices={devices} />
        <TransferQueue transfers={transfers} />
      </div>

      {showSettings && <Settings onClose={() => setShowSettings(false)} />}
    </div>
  )
}

export default App
