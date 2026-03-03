import { useState } from 'react'
import FileSender from './FileSender'
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
}

interface Props {
  devices: Device[]
}

function DeviceList({ devices }: Props) {
  const [selectedDevice, setSelectedDevice] = useState<Device | null>(null)

  const handleSendFile = (device: Device) => {
    setSelectedDevice(device)
  }

  const handleCloseSender = () => {
    setSelectedDevice(null)
  }

  return (
    <>
      <div className="device-list">
        <div className="device-list-header">
          <h2>在线设备 ({devices.length})</h2>
        </div>
        <div className="device-list-content">
          {devices.length === 0 ? (
            <div className="empty-state">
              <p>暂无在线设备</p>
              <p className="hint">等待设备发现...</p>
            </div>
          ) : (
            devices.map((device) => (
              <div key={device.instance_id} className="device-item">
                <div className="device-info">
                  <div className="device-status online"></div>
                  <div className="device-details">
                    <div className="device-name">
                      {device.device_name}
                      {device.instance_name && (
                        <span className="instance-name"> - {device.instance_name}</span>
                      )}
                      {device.is_self && (
                        <span className="self-badge"> (本机)</span>
                      )}
                    </div>
                    <div className="device-address">
                      {device.ip}:{device.port}
                    </div>
                  </div>
                </div>
                <button
                  className="send-button"
                  onClick={() => handleSendFile(device)}
                  disabled={device.is_self}
                >
                  发送文件
                </button>
              </div>
            ))
          )}
        </div>
      </div>

      {selectedDevice && (
        <FileSender device={selectedDevice} onClose={handleCloseSender} />
      )}
    </>
  )
}

export default DeviceList
