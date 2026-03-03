import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './Settings.css'

interface Config {
  instance: {
    instance_id: string
    instance_name: string
    device_id: string
    device_name: string
  }
  network: {
    discovery_port: number
    transfer_port: number
    broadcast_interval: number
  }
  transfer: {
    chunk_size: number
    max_concurrent: number
    enable_compression: boolean
  }
  security: {
    require_auth: boolean
    password: string
    allowed_devices: string[]
    enable_tls: boolean
  }
}

interface Props {
  onClose: () => void
}

function Settings({ onClose }: Props) {
  const [config, setConfig] = useState<Config | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)

  useState(() => {
    const loadConfig = async () => {
      try {
        const result = await invoke<Config>('get_config')
        setConfig(result)
      } catch (error) {
        console.error('Failed to load config:', error)
      } finally {
        setLoading(false)
      }
    }
    loadConfig()
  })

  const handleSave = async () => {
    if (!config) return

    setSaving(true)
    try {
      await invoke('update_config', { config })
      alert('配置已保存！需要重启应用生效。')
      onClose()
    } catch (error) {
      console.error('Failed to save config:', error)
      alert(`保存失败: ${error}`)
    } finally {
      setSaving(false)
    }
  }

  if (loading || !config) {
    return (
      <div className="modal-overlay" onClick={onClose}>
        <div className="modal-content settings-modal" onClick={(e) => e.stopPropagation()}>
          <div className="loading">加载中...</div>
        </div>
      </div>
    )
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content settings-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h2>设置</h2>
          <button className="close-button" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="modal-body">
          <div className="settings-section">
            <h3>实例信息</h3>
            <div className="form-group">
              <label>实例名称</label>
              <input
                type="text"
                value={config.instance.instance_name}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    instance: { ...config.instance, instance_name: e.target.value },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>设备名称</label>
              <input
                type="text"
        lue={config.instance.device_name}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    instance: { ...config.instance, device_name: e.target.value },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>实例 ID</label>
              <input type="text" value={config.instance.instance_id} disabled />
            </div>
          </div>

          <div className="settings-section">
            <h3>网络配置</h3>
            <div className="form-group">
              <label>发现端口 (0 = 自动)</label>
              <input
                type="number"
                value={config.network.discovery_port}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    network: {
                      ...config.network,
                      discovery_port: parseInt(e.target.value) || 0,
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>传输端口 (0 = 自动)</label>
              <input
                type="number"
                value={config.network.transfer_port}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    network: {
                      ...config.network,
                      transfer_port: parseInt(e.target.value) || 0,
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>广播间隔 (秒)</label>
              <input
                type="number"
                value={config.network.broadcast_interval}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    network: {
                      ...config.network,
                      broadcast_interval: parseInt(e.target.value) || 5,
                    },
                  })
                }
              />
            </div>
          </div>

          <div className="settings-section">
            <h3>传输配置</h3>
            <div className="form-group">
              <label>块大小 (字节)</label>
              <input
                type="number"
                value={config.transfer.chunk_size}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      chunk_size: parseInt(e.target.value) || 1048576,
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>最大并发数</label>
              <input
                type="number"
                value={config.transfer.max_concurrent}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      max_concurrent: parseInt(e.target.value) || 3,
                    },
                  })
                }
              />
            </div>
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.transfer.enable_compression}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      transfer: {
                        ...config.transfer,
                        enable_compression: e.target.checked,
                      },
                    })
                  }
                />
                <span>启用压缩</span>
              </label>
            </div>
          </div>

          <div className="settings-section">
            <h3>安全配置</h3>
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.security.require_auth}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      security: {
                        ...config.security,
                        require_auth: e.target.checked,
                      },
                    })
                  }
                />
                <span>需要授权</span>
              </label>
            </div>
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.security.enable_tls}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      security: {
                        ...config.security,
                        enable_tls: e.target.checked,
                      },
                    })
                  }
                />
                <span>启用 TLS 加密</span>
              </label>
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button className="cancel-button" onClick={onClose} disabled={saving}>
            取消
          </button>
          <button className="save-button" onClick={handleSave} disabled={saving}>
            {saving ? '保存中...' : '保存'}
          </button>
        </div>
      </div>
    </div>
  )
}

export default Settings
