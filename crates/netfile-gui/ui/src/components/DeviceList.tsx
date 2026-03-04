import { useState } from 'react'
import DeviceModal from './DeviceModal'
import './DeviceList.css'

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

interface Props {
  devices: Device[]
}

const STORAGE_KEY = 'netfile-manual-devices'

function loadManualDevices(): Device[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    return raw ? JSON.parse(raw) : []
  } catch {
    return []
  }
}

function saveManualDevices(devices: Device[]) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(devices))
}

function DeviceList({ devices }: Props) {
  const [selectedDevice, setSelectedDevice] = useState<Device | null>(null)
  const [showManualInput, setShowManualInput] = useState(false)
  const [manualAddr, setManualAddr] = useState('')
  const [manualDevices, setManualDevices] = useState<Device[]>(loadManualDevices)

  const handleCloseSender = () => {
    setSelectedDevice(null)
  }

  const handleAddManual = () => {
    const trimmed = manualAddr.trim()
    if (!trimmed) return
    const lastColon = trimmed.lastIndexOf(':')
    if (lastColon === -1) return
    const ip = trimmed.slice(0, lastColon)
    const port = parseInt(trimmed.slice(lastColon + 1))
    if (!ip || isNaN(port)) return
    const device: Device = {
      device_id: '',
      instance_id: `manual-${trimmed}`,
      device_name: trimmed,
      instance_name: trimmed,
      ip,
      port,
      version: '',
      is_self: false,
    }
    const updated = [...manualDevices.filter(d => d.instance_id !== device.instance_id), device]
    setManualDevices(updated)
    saveManualDevices(updated)
    setSelectedDevice(device)
    setShowManualInput(false)
    setManualAddr('')
  }

  const handleRemoveManual = (instanceId: string, e: React.MouseEvent) => {
    e.stopPropagation()
    const updated = manualDevices.filter(d => d.instance_id !== instanceId)
    setManualDevices(updated)
    saveManualDevices(updated)
  }

  const allDevices = devices.length === 0 && manualDevices.length === 0

  return (
    <>
      <div className="device-list">
        <div className="device-list-header">
          <h2>在线设备 ({devices.length})</h2>
        </div>
        <div className="device-list-content">
          {allDevices ? (
            <div className="empty-state">
              <p>暂无在线设备</p>
              <p className="hint">等待设备发现...</p>
            </div>
          ) : (
            <>
              {devices.map((device) => (
                <div key={device.instance_id} className="device-item" onClick={() => setSelectedDevice(device)}>
                  <div className="device-info">
                    <div className="device-status online"></div>
                    <div className="device-details">
                      <div className="device-name">
                        {device.instance_name}
                        <span className={device.is_self ? 'self-badge' : 'instance-name'}>
                          {' '}({device.is_self ? '本机' : device.ip})
                        </span>
                      </div>
                    </div>
                  </div>
                </div>
              ))}
              {manualDevices.map((device) => (
                <div key={device.instance_id} className="device-item" onClick={() => setSelectedDevice(device)}>
                  <div className="device-info">
                    <div className="device-status online"></div>
                    <div className="device-details">
                      <div className="device-name">
                        {device.instance_name}
                        <span className="manual-badge">手动</span>
                      </div>
                    </div>
                  </div>
                  <button
                    className="remove-manual-button"
                    onClick={(e) => handleRemoveManual(device.instance_id, e)}
                    title="移除"
                  >
                    ×
                  </button>
                </div>
              ))}
            </>
          )}
        </div>
        <div className="device-list-footer">
          {showManualInput ? (
            <div className="manual-connect-row">
              <input
                className="manual-connect-input"
                type="text"
                placeholder="IP:端口"
                value={manualAddr}
                onChange={(e) => setManualAddr(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') handleAddManual() }}
                autoFocus
              />
              <button className="manual-connect-confirm" onClick={handleAddManual}>添加</button>
              <button className="manual-connect-cancel" onClick={() => { setShowManualInput(false); setManualAddr('') }}>取消</button>
            </div>
          ) : (
            <button className="manual-connect-button" onClick={() => setShowManualInput(true)}>
              + 手动添加设备
            </button>
          )}
        </div>
      </div>

      {selectedDevice && (
        <DeviceModal device={selectedDevice} onClose={handleCloseSender} />
      )}
    </>
  )
}

export default DeviceList
