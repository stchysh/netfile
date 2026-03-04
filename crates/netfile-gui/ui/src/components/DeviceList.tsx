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
              <div key={device.instance_id} className="device-item" onClick={() => handleSendFile(device)}>
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
            ))
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
