import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
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
    signal_server_addr: string
  }
  transfer: {
    chunk_size: number
    max_concurrent: number
    enable_compression: boolean
    download_dir: string
    speed_limit_mbps: number
    require_confirmation: boolean
    quic_stream_window_mb: number
    history_page_size: number
    enable_sharing: boolean
    sharing_require_confirm: boolean
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

function formatChunkSize(bytes: number): string {
  if (bytes >= 1048576) return `${(bytes / 1048576).toFixed(0)} MB`
  return `${(bytes / 1024).toFixed(0)} KB`
}

function Settings({ onClose }: Props) {
  const [config, setConfig] = useState<Config | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [myPublicAddr, setMyPublicAddr] = useState<string>('')
  const [signalConnected, setSignalConnected] = useState(false)

  useEffect(() => {
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
    invoke<string | null>('get_my_public_addr').then((addr) => {
      setMyPublicAddr(addr ?? '')
    }).catch(() => {})
    invoke<{ connected: boolean }>('get_signal_status').then((s) => {
      setSignalConnected(s.connected)
    }).catch(() => {})
  }, [])

  const handleSave = async () => {
    if (!config) return

    setSaving(true)
    try {
      await invoke('update_config', { config })
      onClose()
    } catch (error) {
      console.error('Failed to save config:', error)
      alert(`保存失败: ${error}`)
    } finally {
      setSaving(false)
    }
  }

  const handleSignalToggle = async () => {
    if (!config) return
    try {
      if (signalConnected) {
        await invoke('disconnect_signal_server')
        setSignalConnected(false)
      } else {
        await invoke('connect_signal_server', { serverAddr: config.network.signal_server_addr })
        setSignalConnected(true)
      }
    } catch (error) {
      alert(`操作失败: ${error}`)
    }
  }

  const handleBrowseDownloadDir = async () => {
    try {
      const selected = await open({ multiple: false, directory: true })
      if (selected && config) {
        setConfig({
          ...config,
          transfer: { ...config.transfer, download_dir: selected as string },
        })
      }
    } catch (error) {
      console.error('Failed to pick directory:', error)
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
              <div className="name-input-row">
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
                <button
                  className="random-name-button"
                  onClick={async () => {
                    try {
                      const name = await invoke<string>('get_random_name')
                      setConfig({ ...config, instance: { ...config.instance, instance_name: name } })
                    } catch {}
                  }}
                  title="随机名字"
                >
                  随机
                </button>
              </div>
            </div>
            <div className="form-group">
              <label>设备名称</label>
              <input
                type="text"
                value={config.instance.device_name}
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
              <div className="slider-label-row">
                <label>广播间隔</label>
                <span className="slider-value">{config.network.broadcast_interval} 秒</span>
              </div>
              <input
                type="range"
                min={1}
                max={60}
                step={1}
                value={config.network.broadcast_interval}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    network: {
                      ...config.network,
                      broadcast_interval: parseInt(e.target.value),
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <label>信令服务器地址</label>
              <div className="signal-row">
                <input
                  type="text"
                  value={config.network.signal_server_addr}
                  placeholder="host:37200 或 host1:37200,host2:37200"
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      network: { ...config.network, signal_server_addr: e.target.value },
                    })
                  }
                />
                <button className="signal-connect-btn" onClick={handleSignalToggle}>
                  {signalConnected ? '断开' : '连接'}
                </button>
              </div>
              {signalConnected && <span className="signal-status-ok">已连接</span>}
            </div>
            <div className="form-group">
              <label>我的公网传输地址</label>
              <input type="text" value={myPublicAddr || '获取中...'} disabled />
            </div>
          </div>

          <div className="settings-section">
            <h3>传输配置</h3>
            <div className="form-group">
              <label>下载目录</label>
              <div className="dir-input-row">
                <input
                  type="text"
                  value={config.transfer.download_dir}
                  placeholder="留空使用默认目录 (Downloads/NetFile)"
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      transfer: { ...config.transfer, download_dir: e.target.value },
                    })
                  }
                />
                <button className="browse-button" onClick={handleBrowseDownloadDir}>
                  浏览
                </button>
                <button
                  className="reset-button"
                  onClick={() => setConfig({ ...config, transfer: { ...config.transfer, download_dir: '' } })}
                  title="重置为默认目录"
                >
                  重置
                </button>
              </div>
            </div>
            <div className="form-group">
              <div className="slider-label-row">
                <label>块大小</label>
                <span className="slider-value">{formatChunkSize(config.transfer.chunk_size)}</span>
              </div>
              <input
                type="range"
                min={65536}
                max={16777216}
                step={65536}
                value={config.transfer.chunk_size}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      chunk_size: parseInt(e.target.value),
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <div className="slider-label-row">
                <label>QUIC 流控窗口 <span style={{fontSize:'0.8em', color:'var(--text-muted, #888)'}}>重启后生效</span></label>
                <span className="slider-value">{config.transfer.quic_stream_window_mb ?? 32} MB</span>
              </div>
              <input
                type="range"
                min={8}
                max={256}
                step={8}
                value={config.transfer.quic_stream_window_mb ?? 32}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      quic_stream_window_mb: parseInt(e.target.value),
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <div className="slider-label-row">
                <label>每页记录数</label>
                <span className="slider-value">{config.transfer.history_page_size ?? 20}</span>
              </div>
              <input
                type="range"
                min={5}
                max={200}
                step={5}
                value={config.transfer.history_page_size ?? 20}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      history_page_size: parseInt(e.target.value),
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <div className="slider-label-row">
                <label>最大并发数</label>
                <span className="slider-value">{config.transfer.max_concurrent}</span>
              </div>
              <input
                type="range"
                min={1}
                max={16}
                step={1}
                value={config.transfer.max_concurrent}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      max_concurrent: parseInt(e.target.value),
                    },
                  })
                }
              />
            </div>
            <div className="form-group">
              <div className="slider-label-row">
                <label>传输速度上限</label>
                <span className="slider-value">
                  {config.transfer.speed_limit_mbps === 0
                    ? '不限'
                    : `${config.transfer.speed_limit_mbps} MB/s`}
                </span>
              </div>
              <input
                type="range"
                min={0}
                max={500}
                step={1}
                value={config.transfer.speed_limit_mbps}
                onChange={(e) =>
                  setConfig({
                    ...config,
                    transfer: {
                      ...config.transfer,
                      speed_limit_mbps: parseInt(e.target.value),
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
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.transfer.require_confirmation}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      transfer: {
                        ...config.transfer,
                        require_confirmation: e.target.checked,
                      },
                    })
                  }
                />
                <span>需要确认接收</span>
              </label>
            </div>
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.transfer.enable_sharing ?? true}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      transfer: {
                        ...config.transfer,
                        enable_sharing: e.target.checked,
                      },
                    })
                  }
                />
                <span>启用文件共享</span>
              </label>
            </div>
            <div className="form-group checkbox-group">
              <label>
                <input
                  type="checkbox"
                  checked={config.transfer.sharing_require_confirm ?? false}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      transfer: {
                        ...config.transfer,
                        sharing_require_confirm: e.target.checked,
                      },
                    })
                  }
                />
                <span>共享下载需要确认</span>
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

        <div className="settings-section">
          <h3>诊断</h3>
          <div className="form-group">
            <label>日志导出</label>
            <button
              className="browse-button"
              onClick={async () => {
                try {
                  const path = await invoke<string>('export_diagnostics')
                  alert(`诊断日志已导出: ${path}`)
                } catch (e) {
                  alert(`导出失败: ${e}`)
                }
              }}
            >
              导出诊断日志
            </button>
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
