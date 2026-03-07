import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './TransferHistory.css'

interface TransferRecord {
  id: string
  file_name: string
  file_size: number
  direction: string
  status: string
  error?: string
  timestamp: number
  elapsed_secs: number
  save_path?: string
  transfer_method?: string
}

function methodLabel(method?: string): string {
  if (method === 'lan') return 'LAN'
  if (method === 'iroh') return 'NAT'
  return ''
}

function TransferHistory() {
  const [records, setRecords] = useState<TransferRecord[]>([])
  const lastJsonRef = useRef<string>('')

  useEffect(() => {
    const load = async () => {
      try {
        const result = await invoke<TransferRecord[]>('get_transfer_history')
        const json = JSON.stringify(result)
        if (json !== lastJsonRef.current) {
          lastJsonRef.current = json
          setRecords(result)
        }
      } catch (error) {
        console.error('Failed to load transfer history:', error)
      }
    }
    load()
    const interval = setInterval(load, 3000)
    return () => clearInterval(interval)
  }, [])

  const handleOpenFile = async (path: string) => {
    try {
      await invoke('open_file', { path })
    } catch (error) {
      console.error('Failed to open file:', error)
    }
  }

  const handleOpenFolder = async (path: string, isFolder: boolean) => {
    try {
      if (isFolder) {
        await invoke('open_folder', { path })
      } else {
        const sep = path.includes('\\') ? '\\' : '/'
        const folderPath = path.substring(0, path.lastIndexOf(sep))
        await invoke('open_folder', { path: folderPath })
      }
    } catch (error) {
      console.error('Failed to open folder:', error)
    }
  }

  const handleClear = async () => {
    try {
      await invoke('clear_transfer_history')
      setRecords([])
      lastJsonRef.current = '[]'
    } catch (error) {
      console.error('Failed to clear history:', error)
    }
  }

  const formatSize = (bytes: number): string => {
    if (bytes === 0) return '0 B'
    const k = 1024
    const sizes = ['B', 'KB', 'MB', 'GB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return `${(bytes / Math.pow(k, i)).toFixed(2)} ${sizes[i]}`
  }

  const formatElapsed = (secs: number): string => {
    if (secs === 0) return ''
    if (secs >= 3600) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`
    if (secs >= 60) return `${Math.floor(secs / 60)}m ${secs % 60}s`
    return `${secs}s`
  }

  const formatTime = (timestamp: number): string => {
    const date = new Date(timestamp * 1000)
    const mo = (date.getMonth() + 1).toString().padStart(2, '0')
    const d = date.getDate().toString().padStart(2, '0')
    const h = date.getHours().toString().padStart(2, '0')
    const m = date.getMinutes().toString().padStart(2, '0')
    return `${mo}-${d} ${h}:${m}`
  }

  return (
    <div className="transfer-history">
      <div className="transfer-history-header">
        <h2>传输记录</h2>
        {records.length > 0 && (
          <button className="history-clear-button" onClick={handleClear}>
            清空
          </button>
        )}
      </div>
      <div className="transfer-history-content">
        {records.length === 0 ? (
          <div className="empty-state">
            <p>暂无传输记录</p>
          </div>
        ) : (
          records.map((record) => (
            <div
              key={record.id}
              className={`history-item ${record.status === 'failed' ? 'history-item-error' : ''}`}
            >
              <div className="history-item-header">
                <div className="history-name-row">
                  <span className={`direction-badge direction-${record.direction}`}>
                    {record.direction === 'send' ? '发送' : '接收'}
                  </span>
                  {methodLabel(record.transfer_method) && (
                    <span className="method-badge">{methodLabel(record.transfer_method)}</span>
                  )}
                  <div className="history-file-name">{record.file_name}</div>
                </div>
                <span className={`history-status-label ${record.status === 'failed' ? 'status-failed' : 'status-completed'}`}>
                  {record.status === 'failed' ? '失败' : '完成'}
                </span>
              </div>
              {record.error && (
                <div className="history-error-msg">{record.error}</div>
              )}
              {record.save_path && record.status === 'completed' && (
                <div className="history-open-actions">
                  {!record.file_name.endsWith('/') && (
                    <button className="history-open-btn" onClick={() => handleOpenFile(record.save_path!)}>
                      打开文件
                    </button>
                  )}
                  <button className="history-open-btn" onClick={() => handleOpenFolder(record.save_path!, record.file_name.endsWith('/'))}>
                    打开文件夹
                  </button>
                </div>
              )}
              <div className="history-item-meta">
                <span className="history-size">{formatSize(record.file_size)}</span>
                <span className="history-meta-right">
                  {record.elapsed_secs > 0 && (
                    <span className="history-elapsed">{formatElapsed(record.elapsed_secs)}</span>
                  )}
                  <span className="history-time">{formatTime(record.timestamp)}</span>
                </span>
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  )
}

export default TransferHistory
