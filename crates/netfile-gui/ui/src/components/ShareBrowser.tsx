import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import './ShareBrowser.css'

interface SharedFileInfo {
  file_id: string
  file_name: string
  file_size: number
  file_md5?: string
  save_path?: string
  tags: string[]
  remark: string
  download_count: number
  require_confirm: boolean
  timestamp: number
}

interface DeviceShares {
  instance_id: string
  instance_name: string
  transfer_addr: string
  require_confirm: boolean
  files: SharedFileInfo[]
  loaded: boolean
  error?: string
  is_self?: boolean
}

interface BookmarkEntry {
  id: string
  file_id: string
  file_name: string
  file_size: number
  file_md5?: string
  tags: string[]
  remark: string
  source_instance_id: string
  source_instance_name: string
  source_transfer_addr: string
  require_confirm: boolean
  bookmarked_at: number
}

const SHARE_VIEW_KEY = 'share_browser_view'

function ShareBrowser() {
  const [devices, setDevices] = useState<DeviceShares[]>([])
  const [loading, setLoading] = useState(false)
  const [searchQuery, setSearchQuery] = useState('')
  const [selectedTag, setSelectedTag] = useState<string | null>(null)
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({})
  const [view, setView] = useState<'all' | 'bookmarks'>(
    () => (localStorage.getItem(SHARE_VIEW_KEY) as 'all' | 'bookmarks') || 'all'
  )
  const [bookmarks, setBookmarks] = useState<BookmarkEntry[]>([])
  const lastLoadRef = useRef<number>(0)
  const cacheRef = useRef<DeviceShares[]>([])

  const loadShares = async () => {
    const now = Date.now()
    if (now - lastLoadRef.current < 30000 && cacheRef.current.length > 0) {
      setDevices(cacheRef.current)
      return
    }
    setLoading(true)
    try {
      const result = await invoke<DeviceShares[]>('query_all_shares')
      cacheRef.current = result
      lastLoadRef.current = Date.now()
      setDevices(result)
    } catch (error) {
      console.error('Failed to load shares:', error)
    } finally {
      setLoading(false)
    }
  }

  const loadBookmarks = async () => {
    try {
      const bms = await invoke<BookmarkEntry[]>('get_bookmarks')
      setBookmarks(bms)
    } catch (error) {
      console.error('Failed to load bookmarks:', error)
    }
  }

  useEffect(() => {
    loadShares()
    loadBookmarks()
  }, [])

  const handleRefresh = () => {
    lastLoadRef.current = 0
    cacheRef.current = []
    loadShares()
  }

  const handleDownload = async (device: DeviceShares, file: SharedFileInfo) => {
    try {
      await invoke('request_file_download', {
        transferAddr: device.transfer_addr,
        fileMd5: file.file_md5 ?? '',
        fileId: file.file_id || null,
      })
    } catch (error) {
      console.error('Failed to initiate download:', error)
    }
  }

  const handleSaveAs = async (device: DeviceShares, file: SharedFileInfo) => {
    const destDir = await open({ directory: true, multiple: false })
    if (!destDir) return
    const dir = typeof destDir === 'string' ? destDir : (destDir as string[])[0]
    if (device.is_self && file.save_path) {
      try {
        await invoke('copy_local_file', { srcPath: file.save_path, destDir: dir })
      } catch (error) {
        console.error('Failed to copy local file:', error)
      }
    } else {
      try {
        await invoke('request_file_download_to', {
          transferAddr: device.transfer_addr,
          fileMd5: file.file_md5 ?? '',
          fileId: file.file_id || null,
          saveDir: dir,
        })
      } catch (error) {
        console.error('Failed to initiate download to dir:', error)
      }
    }
  }

  const handleBookmark = async (device: DeviceShares, file: SharedFileInfo) => {
    try {
      const entry: BookmarkEntry = {
        id: `${device.instance_id}-${file.file_id}`,
        file_id: file.file_id,
        file_name: file.file_name,
        file_size: file.file_size,
        file_md5: file.file_md5,
        tags: file.tags,
        remark: file.remark,
        source_instance_id: device.instance_id,
        source_instance_name: device.instance_name,
        source_transfer_addr: device.transfer_addr,
        require_confirm: file.require_confirm,
        bookmarked_at: Math.floor(Date.now() / 1000),
      }
      await invoke('add_bookmark', { entry })
      await loadBookmarks()
    } catch (error) {
      console.error('Failed to add bookmark:', error)
    }
  }

  const handleRemoveBookmark = async (id: string) => {
    try {
      await invoke('remove_bookmark', { id })
      setBookmarks((prev) => prev.filter((b) => b.id !== id))
    } catch (error) {
      console.error('Failed to remove bookmark:', error)
    }
  }

  const handleBookmarkDownload = async (bm: BookmarkEntry) => {
    try {
      await invoke('request_file_download', {
        transferAddr: bm.source_transfer_addr,
        fileMd5: bm.file_md5 ?? '',
        fileId: bm.file_id || null,
      })
    } catch (error) {
      console.error('Failed to download bookmark:', error)
    }
  }

  const handleBookmarkSaveAs = async (bm: BookmarkEntry) => {
    const destDir = await open({ directory: true, multiple: false })
    if (!destDir) return
    const dir = typeof destDir === 'string' ? destDir : (destDir as string[])[0]
    try {
      await invoke('request_file_download_to', {
        transferAddr: bm.source_transfer_addr,
        fileMd5: bm.file_md5 ?? '',
        fileId: bm.file_id || null,
        saveDir: dir,
      })
    } catch (error) {
      console.error('Failed to save bookmark as:', error)
    }
  }

  const formatSize = (bytes: number): string => {
    if (bytes === 0) return '0 B'
    const k = 1024
    const sizes = ['B', 'KB', 'MB', 'GB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return `${(bytes / Math.pow(k, i)).toFixed(2)} ${sizes[i]}`
  }

  function deduplicateByMd5(files: SharedFileInfo[]): SharedFileInfo[] {
    const md5Map = new Map<string, SharedFileInfo>()
    const noMd5: SharedFileInfo[] = []
    for (const file of files) {
      if (!file.file_md5) { noMd5.push(file); continue }
      if (md5Map.has(file.file_md5)) {
        const ex = md5Map.get(file.file_md5)!
        if (!ex.file_name.split(' / ').includes(file.file_name))
          ex.file_name = ex.file_name + ' / ' + file.file_name
        for (const tag of file.tags)
          if (!ex.tags.includes(tag)) ex.tags.push(tag)
      } else {
        md5Map.set(file.file_md5, { ...file })
      }
    }
    return [...md5Map.values(), ...noMd5]
  }

  const allTags = Array.from(new Set(
    devices.flatMap((d) => d.files.flatMap((f) => f.tags))
  ))

  const filteredDevices = devices.map((d) => ({
    ...d,
    files: deduplicateByMd5(d.files.filter((f) => {
      const matchSearch = !searchQuery || f.file_name.toLowerCase().includes(searchQuery.toLowerCase())
      const matchTag = !selectedTag || f.tags.includes(selectedTag)
      return matchSearch && matchTag
    })),
  })).filter((d) => d.files.length > 0 || !searchQuery)

  const bookmarkIds = new Set(bookmarks.map((b) => `${b.source_instance_id}-${b.file_id}`))

  return (
    <div className="share-browser">
      <div className="share-browser-header">
        <div className="share-header-top">
          <div className="share-view-tabs">
            <button
              className={`share-view-tab ${view === 'all' ? 'active' : ''}`}
              onClick={() => { setView('all'); localStorage.setItem(SHARE_VIEW_KEY, 'all') }}
            >
              全部
            </button>
            <button
              className={`share-view-tab ${view === 'bookmarks' ? 'active' : ''}`}
              onClick={() => { setView('bookmarks'); localStorage.setItem(SHARE_VIEW_KEY, 'bookmarks') }}
            >
              收藏夹
            </button>
          </div>
          <input
            className="share-search-input"
            placeholder="搜索文件名"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
          />
          <button className="share-refresh-btn" onClick={handleRefresh} disabled={loading}>
            {loading ? '加载中...' : '刷新'}
          </button>
        </div>
        {allTags.length > 0 && view === 'all' && (
          <div className="share-tag-filters">
            <button
              className={`share-tag-chip ${!selectedTag ? 'active' : ''}`}
              onClick={() => setSelectedTag(null)}
            >
              全部
            </button>
            {allTags.map((tag) => (
              <button
                key={tag}
                className={`share-tag-chip ${selectedTag === tag ? 'active' : ''}`}
                onClick={() => setSelectedTag(selectedTag === tag ? null : tag)}
              >
                {tag}
              </button>
            ))}
          </div>
        )}
      </div>

      <div className="share-browser-content">
        {view === 'bookmarks' ? (
          bookmarks.length === 0 ? (
            <div className="empty-state"><p>暂无收藏</p></div>
          ) : (
            bookmarks.map((bm) => (
              <div key={bm.id} className="share-file-item">
                <div className="share-file-name">{bm.file_name}</div>
                {bm.remark && <div className="share-file-remark">{bm.remark}</div>}
                <div className="share-file-meta">
                  <span className="share-file-size">{formatSize(bm.file_size)}</span>
                  <span className="share-file-source">来自：{bm.source_instance_name}</span>
                  {bm.tags.length > 0 && (
                    <span className="share-file-tags">
                      {bm.tags.map((t) => <span key={t} className="share-tag">{t}</span>)}
                    </span>
                  )}
                  {bm.require_confirm && <span className="share-confirm-badge">需确认</span>}
                </div>
                <div className="share-file-actions">
                  <button
                    className="share-action-btn share-download-btn"
                    onClick={() => handleBookmarkDownload(bm)}
                  >
                    下载
                  </button>
                  <button
                    className="share-action-btn"
                    onClick={() => handleBookmarkSaveAs(bm)}
                  >
                    另存为
                  </button>
                  <button
                    className="share-action-btn"
                    onClick={() => handleRemoveBookmark(bm.id)}
                  >
                    取消收藏
                  </button>
                </div>
              </div>
            ))
          )
        ) : (
          filteredDevices.length === 0 ? (
            <div className="empty-state">
              <p>{loading ? '正在加载...' : '暂无共享文件'}</p>
            </div>
          ) : (
            filteredDevices.map((device) => (
              <div key={device.instance_id} className="share-device-group">
                <div
                  className="share-device-header"
                  onClick={() => setCollapsed((prev) => ({ ...prev, [device.instance_id]: !prev[device.instance_id] }))}
                >
                  <span className="share-device-toggle">
                    {collapsed[device.instance_id] ? '▶' : '▼'}
                  </span>
                  <span className="share-device-name">{device.instance_name}</span>
                  <span className="share-device-count">（{device.files.length} 个文件）</span>
                  {!device.loaded && <span className="share-device-error">{device.error || '连接失败'}</span>}
                </div>
                {!collapsed[device.instance_id] && device.files.map((file) => {
                  const bmKey = `${device.instance_id}-${file.file_id}`
                  const isBookmarked = bookmarkIds.has(bmKey)
                  return (
                    <div key={file.file_id} className="share-file-item">
                      <div className="share-file-name">{file.file_name}</div>
                      {file.remark && <div className="share-file-remark">{file.remark}</div>}
                      <div className="share-file-meta">
                        <span className="share-file-size">{formatSize(file.file_size)}</span>
                        {file.tags.length > 0 && (
                          <span className="share-file-tags">
                            {file.tags.map((t) => <span key={t} className="share-tag">{t}</span>)}
                          </span>
                        )}
                        {file.download_count > 0 && (
                          <span className="share-file-heat">热度：{file.download_count}</span>
                        )}
                        {file.require_confirm && <span className="share-confirm-badge">需确认</span>}
                      </div>
                      <div className="share-file-actions">
                        <button
                          className="share-action-btn share-download-btn"
                          onClick={() => handleDownload(device, file)}
                        >
                          下载
                        </button>
                        <button
                          className="share-action-btn"
                          onClick={() => handleSaveAs(device, file)}
                          disabled={!file.file_md5 && !(device.is_self && file.save_path)}
                        >
                          另存为
                        </button>
                        <button
                          className={`share-action-btn ${isBookmarked ? 'share-bookmarked-btn' : ''}`}
                          onClick={() => !isBookmarked && handleBookmark(device, file)}
                          disabled={isBookmarked}
                        >
                          {isBookmarked ? '已收藏' : '收藏'}
                        </button>
                      </div>
                    </div>
                  )
                })}
              </div>
            ))
          )
        )}
      </div>
    </div>
  )
}

export default ShareBrowser
