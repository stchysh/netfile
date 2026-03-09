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

interface ShareEntry {
  record_id: string
  file_name: string
  file_size: number
  save_path: string
  file_md5?: string
  tags: string[]
  remark: string
  excluded: boolean
  download_count: number
  timestamp: number
  file_exists: boolean
}

function methodLabel(method?: string): string {
  if (method === 'lan') return 'LAN'
  if (method === 'iroh') return 'NAT'
  return ''
}

function TagEditor({
  tags,
  onChange,
}: {
  tags: string[]
  onChange: (tags: string[]) => void
}) {
  const [input, setInput] = useState('')

  const addTag = () => {
    const t = input.trim()
    if (!t || tags.includes(t) || tags.length >= 10) return
    onChange([...tags, t])
    setInput('')
  }

  const removeTag = (tag: string) => {
    onChange(tags.filter((t) => t !== tag))
  }

  return (
    <div className="tag-editor">
      {tags.map((tag) => (
        <span key={tag} className="tag-chip">
          {tag}
          <button className="tag-remove" onClick={() => removeTag(tag)}>×</button>
        </span>
      ))}
      <input
        className="tag-input"
        placeholder="添加标签"
        value={input}
        onChange={(e) => setInput(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') { e.preventDefault(); addTag() }
        }}
        maxLength={32}
      />
    </div>
  )
}

function TransferHistory() {
  const [records, setRecords] = useState<TransferRecord[]>([])
  const lastJsonRef = useRef<string>('')
  const [searchQuery, setSearchQuery] = useState('')
  const [pageSize, setPageSize] = useState(20)
  const [currentPage, setCurrentPage] = useState(0)
  const [shareEntries, setShareEntries] = useState<Record<string, ShareEntry>>({})
  const lastShareJsonRef = useRef<string>('')

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
    const loadShares = async () => {
      try {
        const entries = await invoke<ShareEntry[]>('get_share_entries')
        const json = JSON.stringify(entries)
        if (json !== lastShareJsonRef.current) {
          lastShareJsonRef.current = json
          const map: Record<string, ShareEntry> = {}
          for (const e of entries) {
            map[e.record_id] = e
          }
          setShareEntries(map)
        }
      } catch (error) {
        console.error('Failed to load share entries:', error)
      }
    }
    const loadConfig = async () => {
      try {
        const config = await invoke<any>('get_config')
        setPageSize(config?.transfer?.history_page_size ?? 20)
      } catch (error) {
        console.error('Failed to load config:', error)
      }
    }
    load()
    loadShares()
    loadConfig()
    const interval = setInterval(() => { load(); loadShares() }, 3000)
    return () => clearInterval(interval)
  }, [])

  useEffect(() => {
    setCurrentPage(0)
  }, [searchQuery])

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

  const handleDelete = async (id: string) => {
    try {
      await invoke('delete_transfer_record', { id })
      setRecords((prev) => prev.filter((r) => r.id !== id))
      setShareEntries((prev) => {
        const next = { ...prev }
        delete next[id]
        return next
      })
    } catch (error) {
      console.error('Failed to delete record:', error)
    }
  }

  const handleClearFiltered = async (ids: string[]) => {
    try {
      await invoke('delete_transfer_records', { ids })
      const idSet = new Set(ids)
      setRecords((prev) => prev.filter((r) => !idSet.has(r.id)))
      setShareEntries((prev) => {
        const next = { ...prev }
        for (const id of ids) delete next[id]
        return next
      })
    } catch (error) {
      console.error('Failed to delete filtered records:', error)
    }
  }

  const handleToggleExcluded = async (recordId: string, current: boolean) => {
    try {
      await invoke('set_share_excluded', { recordId, excluded: !current })
      setShareEntries((prev) => {
        if (!prev[recordId]) return prev
        const targetMd5 = prev[recordId].file_md5
        const next = { ...prev }
        for (const id of Object.keys(next)) {
          if (id === recordId || (targetMd5 && next[id].file_md5 === targetMd5)) {
            next[id] = { ...next[id], excluded: !current }
          }
        }
        return next
      })
    } catch (error) {
      console.error('Failed to toggle share excluded:', error)
    }
  }

  const handleUpdateTags = async (recordId: string, tags: string[]) => {
    try {
      await invoke('update_share_tags', { recordId, tags })
      setShareEntries((prev) => {
        if (!prev[recordId]) return prev
        return { ...prev, [recordId]: { ...prev[recordId], tags } }
      })
    } catch (error) {
      console.error('Failed to update tags:', error)
    }
  }

  const handleUpdateRemark = async (recordId: string, remark: string) => {
    try {
      await invoke('update_share_remark', { recordId, remark })
      setShareEntries((prev) => {
        if (!prev[recordId]) return prev
        return { ...prev, [recordId]: { ...prev[recordId], remark } }
      })
    } catch (error) {
      console.error('Failed to update remark:', error)
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

  const filteredRecords = records.filter(r =>
    r.file_name.toLowerCase().includes(searchQuery.toLowerCase())
  )
  const totalPages = Math.ceil(filteredRecords.length / pageSize)
  const pagedRecords = filteredRecords.slice(currentPage * pageSize, (currentPage + 1) * pageSize)

  return (
    <div className="transfer-history">
      <div className="transfer-history-header">
        <h2>传输记录</h2>
        <input
          className="history-search-input"
          placeholder="搜索文件名"
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
        />
        {records.length > 0 && (
          searchQuery ? (
            <button
              className="history-clear-button"
              onClick={() => handleClearFiltered(filteredRecords.map((r) => r.id))}
              disabled={filteredRecords.length === 0}
            >
              清空筛选
            </button>
          ) : (
            <button className="history-clear-button" onClick={handleClear}>
              清空
            </button>
          )
        )}
      </div>
      <div className="transfer-history-content">
        {filteredRecords.length === 0 ? (
          <div className="empty-state">
            <p>暂无传输记录</p>
          </div>
        ) : (
          <>
          {pagedRecords.map((record) => {
            const shareEntry = shareEntries[record.id]
            return (
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
                {record.save_path && record.status === 'completed' && (
                  <>
                    {!record.file_name.endsWith('/') && (
                      <button className="history-open-btn" onClick={() => handleOpenFile(record.save_path!)}>
                        打开文件
                      </button>
                    )}
                    <button className="history-open-btn" onClick={() => handleOpenFolder(record.save_path!, record.file_name.endsWith('/'))}>
                      打开文件夹
                    </button>
                  </>
                )}
                <button
                  className="history-delete-btn"
                  onClick={() => handleDelete(record.id)}
                >
                  删除
                </button>
              </div>
              {record.error && (
                <div className="history-error-msg">{record.error}</div>
              )}
              {record.status === 'completed' && shareEntry && (
                <div className="share-meta-section">
                  <div className="share-remark-row">
                    <input
                      className="share-remark-input"
                      placeholder="添加备注（最多100字）"
                      value={shareEntry.remark}
                      maxLength={100}
                      onChange={(e) => {
                        const v = e.target.value
                        setShareEntries((prev) => ({
                          ...prev,
                          [record.id]: { ...prev[record.id], remark: v },
                        }))
                      }}
                      onBlur={() => handleUpdateRemark(record.id, shareEntry.remark)}
                    />
                    {!shareEntry.file_exists ? (
                      <span className="share-file-missing">文件不存在</span>
                    ) : (
                      <button
                        className={`share-exclude-btn ${shareEntry.excluded ? 'share-excluded' : ''}`}
                        onClick={() => handleToggleExcluded(record.id, shareEntry.excluded)}
                      >
                        {shareEntry.excluded ? '已不共享' : '共享中'}
                      </button>
                    )}
                  </div>
                  <TagEditor
                    tags={shareEntry.tags}
                    onChange={(tags) => handleUpdateTags(record.id, tags)}
                  />
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
          )})}
          {totalPages > 1 && (
            <div className="history-pagination">
              <button
                className="history-page-btn"
                disabled={currentPage === 0}
                onClick={() => setCurrentPage(p => p - 1)}
              >
                上一页
              </button>
              <span>第 {currentPage + 1} 页 / 共 {totalPages} 页</span>
              <button
                className="history-page-btn"
                disabled={currentPage >= totalPages - 1}
                onClick={() => setCurrentPage(p => p + 1)}
              >
                下一页
              </button>
            </div>
          )}
          </>
        )}
      </div>
    </div>
  )
}

export default TransferHistory
