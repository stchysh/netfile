import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import DeviceModal from './DeviceModal'
import InviteDialog from './InviteDialog'
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

interface FriendInfo {
  device_id: string
  instance_name: string
  online: boolean
  transfer_addr: string | null
}

interface DeviceAlias {
  alias: string
  favorite: boolean
}

interface Props {
  devices: Device[]
}

const STORAGE_KEY = 'netfile-manual-devices'
const LAST_READ_KEY = 'netfile-last-read-counts'

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

function loadLastReadCounts(): Record<string, number> {
  try {
    const raw = localStorage.getItem(LAST_READ_KEY)
    return raw ? JSON.parse(raw) : {}
  } catch {
    return {}
  }
}

function saveLastReadCounts(counts: Record<string, number>) {
  localStorage.setItem(LAST_READ_KEY, JSON.stringify(counts))
}

function DeviceList({ devices }: Props) {
  const [selectedDevice, setSelectedDevice] = useState<Device | null>(null)
  const [showManualInput, setShowManualInput] = useState(false)
  const [manualAddr, setManualAddr] = useState('')
  const [manualDevices, setManualDevices] = useState<Device[]>(loadManualDevices)
  const [unreadIds, setUnreadIds] = useState<Set<string>>(new Set())
  const [activeChatDeviceId, setActiveChatDeviceId] = useState<string | null>(null)
  const [signalFriends, setSignalFriends] = useState<FriendInfo[]>([])
  const [showInviteDialog, setShowInviteDialog] = useState(false)
  const [aliases, setAliases] = useState<Record<string, DeviceAlias>>({})
  const [editingAliasId, setEditingAliasId] = useState<string | null>(null)
  const [aliasInput, setAliasInput] = useState('')

  const msgCountsRef = useRef<Record<string, number>>({})
  const lastReadRef = useRef<Record<string, number>>(loadLastReadCounts())
  const activeChatRef = useRef<string | null>(null)

  activeChatRef.current = activeChatDeviceId

  useEffect(() => {
    const pollCounts = async () => {
      try {
        const counts = await invoke<Record<string, number>>('get_message_counts')
        msgCountsRef.current = counts

        if (activeChatRef.current) {
          const id = activeChatRef.current
          if (counts[id] !== undefined) {
            lastReadRef.current[id] = counts[id]
            saveLastReadCounts(lastReadRef.current)
          }
        }

        const newUnread = new Set<string>()
        for (const [id, count] of Object.entries(counts)) {
          if (id !== activeChatRef.current && count > (lastReadRef.current[id] ?? 0)) {
            newUnread.add(id)
          }
        }
        setUnreadIds(prev => {
          const prevArr = Array.from(prev).sort().join(',')
          const nextArr = Array.from(newUnread).sort().join(',')
          return prevArr === nextArr ? prev : newUnread
        })
      } catch {
        // ignore
      }
    }
    pollCounts()
    const interval = setInterval(pollCounts, 2000)
    return () => clearInterval(interval)
  }, [])

  useEffect(() => {
    const pollFriends = async () => {
      try {
        const friends = await invoke<FriendInfo[]>('get_signal_friends')
        setSignalFriends(prev => {
          const prevStr = JSON.stringify(prev)
          const nextStr = JSON.stringify(friends)
          return prevStr === nextStr ? prev : friends
        })
      } catch {
        // ignore
      }
    }
    pollFriends()
    const interval = setInterval(pollFriends, 2000)
    return () => clearInterval(interval)
  }, [])

  useEffect(() => {
    const fetchAliases = async () => {
      try {
        const result = await invoke<Record<string, DeviceAlias>>('get_device_aliases')
        setAliases(prev => JSON.stringify(prev) === JSON.stringify(result) ? prev : result)
      } catch { /* ignore */ }
    }
    fetchAliases()
    const interval = setInterval(fetchAliases, 5000)
    return () => clearInterval(interval)
  }, [])

  const displayName = (deviceId: string, fallback: string) => {
    const a = aliases[deviceId]
    return a?.alias ? a.alias : fallback
  }

  const isFavorite = (deviceId: string) => aliases[deviceId]?.favorite ?? false

  const saveAlias = async (deviceId: string, alias: string) => {
    try {
      await invoke('set_device_alias', { deviceId, alias })
      setAliases(prev => ({ ...prev, [deviceId]: { ...prev[deviceId], alias } }))
    } catch { /* ignore */ }
    setEditingAliasId(null)
  }

  const toggleFavorite = async (deviceId: string, e: React.MouseEvent) => {
    e.stopPropagation()
    const current = aliases[deviceId]?.favorite ?? false
    try {
      await invoke('set_device_favorite', { deviceId, favorite: !current })
      setAliases(prev => ({ ...prev, [deviceId]: { ...prev[deviceId] ?? { alias: '' }, favorite: !current } }))
    } catch { /* ignore */ }
  }

  const handleBroadcast = async () => {
    try {
      const selected = await open({ multiple: false, directory: false })
      if (!selected) return
      const filePath = selected as string
      const count = await invoke<number>('send_file_broadcast', { filePath })
      if (count === 0) {
        alert('没有可广播的局域网设备')
      }
    } catch (e) {
      alert(`广播失败: ${e}`)
    }
  }

  const markRead = (deviceId: string) => {
    if (!deviceId) return
    const current = msgCountsRef.current[deviceId] ?? 0
    lastReadRef.current[deviceId] = current
    saveLastReadCounts(lastReadRef.current)
    setUnreadIds(prev => {
      if (!prev.has(deviceId)) return prev
      const next = new Set(prev)
      next.delete(deviceId)
      return next
    })
  }

  const handleCloseSender = () => {
    setSelectedDevice(null)
    setActiveChatDeviceId(null)
  }

  const handleTabChange = (tab: 'files' | 'chat') => {
    if (!selectedDevice) return
    if (tab === 'chat') {
      setActiveChatDeviceId(selectedDevice.device_id || null)
      markRead(selectedDevice.device_id)
    } else {
      setActiveChatDeviceId(null)
    }
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

  const handleOpenFriend = (f: FriendInfo) => {
    const addr = f.transfer_addr ?? ''
    if (!addr) {
      console.warn('[punch-flow][ui] open friend with empty transfer_addr', {
        deviceId: f.device_id,
        instanceName: f.instance_name,
      })
    } else {
      console.info('[punch-flow][ui] open friend with transfer_addr', {
        deviceId: f.device_id,
        instanceName: f.instance_name,
        transferAddr: addr,
      })
    }
    const colonIdx = addr.lastIndexOf(':')
    const pseudoDevice: Device = {
      device_id: f.device_id,
      instance_id: f.device_id,
      device_name: 'WAN',
      instance_name: f.instance_name,
      ip: colonIdx >= 0 ? addr.slice(0, colonIdx) : addr,
      port: colonIdx >= 0 ? parseInt(addr.slice(colonIdx + 1)) || 0 : 0,
      version: '',
      is_self: false,
    }
    setSelectedDevice(pseudoDevice)
  }

  const allDevices = devices.length === 0 && manualDevices.length === 0 && signalFriends.length === 0

  const sortedDevices = [...devices].sort((a, b) => {
    const af = !a.is_self && a.device_id ? (isFavorite(a.device_id) ? -1 : 0) : 0
    const bf = !b.is_self && b.device_id ? (isFavorite(b.device_id) ? -1 : 0) : 0
    return af - bf
  })

  const renderAliasEditor = (deviceId: string, e: React.MouseEvent) => {
    e.stopPropagation()
    setEditingAliasId(deviceId)
    setAliasInput(aliases[deviceId]?.alias ?? '')
  }

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
              {sortedDevices.map((device) => {
                const hasUnread = !device.is_self && device.device_id
                  && unreadIds.has(device.device_id)
                  && activeChatDeviceId !== device.device_id
                const fav = !device.is_self && device.device_id ? isFavorite(device.device_id) : false
                const isEditing = editingAliasId === device.device_id
                return (
                  <div key={device.instance_id} className="device-item" onClick={() => setSelectedDevice(device)}>
                    <div className="device-info">
                      <div className="device-status online"></div>
                      <div className="device-details">
                        {isEditing ? (
                          <div className="alias-edit-row" onClick={e => e.stopPropagation()}>
                            <input
                              className="alias-input"
                              value={aliasInput}
                              onChange={e => setAliasInput(e.target.value)}
                              onKeyDown={e => { if (e.key === 'Enter') saveAlias(device.device_id, aliasInput); if (e.key === 'Escape') setEditingAliasId(null) }}
                              autoFocus
                            />
                            <button className="alias-save-btn" onClick={() => saveAlias(device.device_id, aliasInput)}>确认</button>
                            <button className="alias-cancel-btn" onClick={() => setEditingAliasId(null)}>取消</button>
                          </div>
                        ) : (
                          <div className="device-name">
                            {displayName(device.device_id, device.instance_name)}
                            <span className={device.is_self ? 'self-badge' : 'instance-name'}>
                              {' '}({device.is_self ? '本机' : device.ip})
                            </span>
                          </div>
                        )}
                      </div>
                    </div>
                    {!device.is_self && device.device_id && (
                      <>
                        <button
                          className={`fav-btn ${fav ? 'fav-btn-active' : ''}`}
                          onClick={e => toggleFavorite(device.device_id, e)}
                          title={fav ? '取消收藏' : '收藏'}
                        >★</button>
                        <button
                          className="alias-btn"
                          onClick={e => renderAliasEditor(device.device_id, e)}
                          title="设置别名"
                        >✎</button>
                      </>
                    )}
                    {hasUnread && <div className="unread-dot"></div>}
                  </div>
                )
              })}
              {signalFriends.length > 0 && (
                <div className="friends-section">
                  <div className="friends-label">网络好友</div>
                  {signalFriends.filter(f => f.online).sort((a, b) => (isFavorite(b.device_id) ? 1 : 0) - (isFavorite(a.device_id) ? 1 : 0)).map(f => {
                    const hasUnread = unreadIds.has(f.device_id) && activeChatDeviceId !== f.device_id
                    const addr = f.transfer_addr ?? ''
                    const colonIdx = addr.lastIndexOf(':')
                    const ip = colonIdx >= 0 ? addr.slice(0, colonIdx) : (addr || '未知')
                    const fav = isFavorite(f.device_id)
                    const isEditing = editingAliasId === f.device_id
                    return (
                      <div className="device-item" key={f.device_id} onClick={() => handleOpenFriend(f)}>
                        <div className="device-info">
                          <div className="device-status online"></div>
                          <div className="device-details">
                            {isEditing ? (
                              <div className="alias-edit-row" onClick={e => e.stopPropagation()}>
                                <input
                                  className="alias-input"
                                  value={aliasInput}
                                  onChange={e => setAliasInput(e.target.value)}
                                  onKeyDown={e => { if (e.key === 'Enter') saveAlias(f.device_id, aliasInput); if (e.key === 'Escape') setEditingAliasId(null) }}
                                  autoFocus
                                />
                                <button className="alias-save-btn" onClick={() => saveAlias(f.device_id, aliasInput)}>确认</button>
                                <button className="alias-cancel-btn" onClick={() => setEditingAliasId(null)}>取消</button>
                              </div>
                            ) : (
                              <div className="device-name">
                                {displayName(f.device_id, f.instance_name)}
                                <span className="instance-name"> ({ip})</span>
                              </div>
                            )}
                          </div>
                        </div>
                        <button
                          className={`fav-btn ${fav ? 'fav-btn-active' : ''}`}
                          onClick={e => toggleFavorite(f.device_id, e)}
                          title={fav ? '取消收藏' : '收藏'}
                        >★</button>
                        <button
                          className="alias-btn"
                          onClick={e => { e.stopPropagation(); setEditingAliasId(f.device_id); setAliasInput(aliases[f.device_id]?.alias ?? '') }}
                          title="设置别名"
                        >✎</button>
                        {hasUnread && <div className="unread-dot"></div>}
                      </div>
                    )
                  })}
                  {signalFriends.filter(f => !f.online).map(f => (
                    <div className="device-item device-item-offline" key={f.device_id}>
                      <div className="device-info">
                        <div className="device-status offline"></div>
                        <div className="device-details">
                          <div className="device-name">
                            {displayName(f.device_id, f.instance_name)}
                            <span className="offline-badge">离线</span>
                          </div>
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}
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
            <div className="device-list-footer-btns">
              <button className="manual-connect-button" onClick={() => setShowManualInput(true)}>
                + 手动添加设备
              </button>
              <button className="invite-btn" onClick={() => setShowInviteDialog(true)}>
                邀请好友
              </button>
              <button className="broadcast-btn" onClick={handleBroadcast} title="发送文件给所有局域网设备">
                广播
              </button>
            </div>
          )}
        </div>
      </div>

      {selectedDevice && (
        <DeviceModal
          device={selectedDevice}
          onClose={handleCloseSender}
          startOnChat={!!(selectedDevice.device_id && unreadIds.has(selectedDevice.device_id))}
          onTabChange={handleTabChange}
        />
      )}
      {showInviteDialog && <InviteDialog onClose={() => setShowInviteDialog(false)} />}
    </>
  )
}

export default DeviceList
