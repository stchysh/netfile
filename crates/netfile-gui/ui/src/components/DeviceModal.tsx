import { useState } from 'react'
import FileSender from './FileSender'
import Chat from './Chat'
import './DeviceModal.css'

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
  device: Device
  onClose: () => void
  onChatRead?: () => void
}

type Tab = 'files' | 'chat'

function DeviceModal({ device, onClose, onChatRead }: Props) {
  const [tab, setTab] = useState<Tab>('files')

  const handleChatTab = () => {
    setTab('chat')
    onChatRead?.()
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="device-modal-content" onClick={(e) => e.stopPropagation()}>
        <div className="device-modal-header">
          <div className="device-modal-title">
            <span className="device-modal-name">{device.instance_name}</span>
            <span className="device-modal-ip">({device.is_self ? '本机' : device.ip})</span>
          </div>
          <div className="device-modal-tabs">
            <button
              className={`tab-button ${tab === 'files' ? 'active' : ''}`}
              onClick={() => setTab('files')}
            >
              文件传输
            </button>
            <button
              className={`tab-button ${tab === 'chat' ? 'active' : ''}`}
              onClick={handleChatTab}
            >
              消息
            </button>
          </div>
          <button className="close-button" onClick={onClose}>
            ×
          </button>
        </div>
        <div className="device-modal-body">
          {tab === 'files' ? (
            <FileSender device={device} onClose={onClose} embedded />
          ) : (
            <Chat device={device} />
          )}
        </div>
      </div>
    </div>
  )
}

export default DeviceModal
