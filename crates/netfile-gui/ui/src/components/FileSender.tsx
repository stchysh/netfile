import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import { listen } from '@tauri-apps/api/event'
import './FileSender.css'

interface Device {
  device_id: string
  instance_id: string
  device_name: string
  instance_name: string
  ip: string
  port: number
}

interface Props {
  device: Device | null
  onClose: () => void
  embedded?: boolean
}

interface SelectedFile {
  path: string
  name: string
  size: number
}

function FileSender({ device, onClose, embedded }: Props) {
  const [selectedFiles, setSelectedFiles] = useState<SelectedFile[]>([])
  const [enableCompression, setEnableCompression] = useState(false)
  const [dragOver, setDragOver] = useState(false)
  const [errorMessage, setErrorMessage] = useState('')

  useEffect(() => {
    const unlisten = listen<{ paths: string[]; position: unknown } | string[]>(
      'tauri://drag-drop',
      (event) => {
        const payload = event.payload
        const paths: string[] = Array.isArray(payload)
          ? payload
          : (payload as { paths: string[] }).paths ?? []

        const newFiles: SelectedFile[] = paths.map((path) => {
          const name = path.split(/[\\/]/).pop() || path
          return { path, name, size: 0 }
        })
        setSelectedFiles((prev) => [...prev, ...newFiles])
      },
    )

    return () => {
      unlisten.then((fn) => fn())
    }
  }, [])

  if (!device) return null

  const handleSelectFile = async () => {
    setErrorMessage('')
    try {
      const selected = await open({
        multiple: true,
        directory: false,
      })

      if (selected) {
        const files = Array.isArray(selected) ? selected : [selected]
        const newFiles: SelectedFile[] = files.map((path) => ({
          path,
          name: path.split(/[\\/]/).pop() || path,
          size: 0,
        }))
        setSelectedFiles((prev) => [...prev, ...newFiles])
      }
    } catch (error) {
      setErrorMessage(`选择文件失败: ${error}`)
    }
  }

  const handleSelectFolder = async () => {
    setErrorMessage('')
    try {
      const selected = await open({
        multiple: false,
        directory: true,
      })

      if (selected) {
        const name = selected.split(/[\\/]/).pop() || selected
        setSelectedFiles((prev) => [
          ...prev,
          { path: selected, name: `${name}/`, size: 0 },
        ])
      }
    } catch (error) {
      setErrorMessage(`选择文件夹失败: ${error}`)
    }
  }

  const handleRemoveFile = (index: number) => {
    setSelectedFiles((prev) => prev.filter((_, i) => i !== index))
  }

  const handleSend = () => {
    if (selectedFiles.length === 0) {
      setErrorMessage('请先选择文件或文件夹')
      return
    }

    const targetAddr = `${device.ip}:${device.port}`
    for (const file of selectedFiles) {
      invoke('send_file', {
        targetAddr,
        filePath: file.path,
        enableCompression,
      })
    }
    onClose()
  }

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(true)
  }

  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(false)
  }

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(false)
  }

  const inner = (
    <>
        <div className="modal-body">
          <div className="target-info">
            <span className="label">发送到:</span>
            <span className="value">
              {device.device_name}
              {device.instance_name && ` - ${device.instance_name}`}
              {` (${device.ip}:${device.port})`}
            </span>
          </div>

          <div
            className={`file-list ${dragOver ? 'drag-over' : ''}`}
            onDragOver={handleDragOver}
            onDragLeave={handleDragLeave}
            onDrop={handleDrop}
          >
            <div className="file-list-header">
              <span>已选择文件:</span>
            </div>
            <div className="file-list-content">
              {selectedFiles.length === 0 ? (
                <div className="empty-state">
                  <p>拖拽文件/文件夹到此处</p>
                  <p className="hint">或点击下方按钮选择</p>
                </div>
              ) : (
                selectedFiles.map((file, index) => (
                  <div key={index} className="file-item">
                    <div className="file-info">
                      <span className="file-icon">
                        {file.name.endsWith('/') ? '📁' : '📄'}
                      </span>
                      <div className="file-details">
                        <div className="file-name">{file.name}</div>
                      </div>
                    </div>
                    <button
                      className="remove-button"
                      onClick={() => handleRemoveFile(index)}
                    >
                      ×
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>

          <div className="action-buttons">
            <button className="add-button" onClick={handleSelectFile}>
              + 添加文件
            </button>
            <button className="add-button" onClick={handleSelectFolder}>
              + 添加文件夹
            </button>
          </div>

          <div className="options">
            <label className="checkbox-label">
              <input
                type="checkbox"
                checked={enableCompression}
                onChange={(e) => setEnableCompression(e.target.checked)}
              />
              <span>启用压缩</span>
            </label>
          </div>
        </div>

        <div className="modal-footer">
          {errorMessage && (
            <div className="error-message">{errorMessage}</div>
          )}
          <div className="footer-buttons">
            <button className="cancel-button" onClick={onClose}>
              取消
            </button>
            <button className="send-button" onClick={handleSend}>
              {`发送${selectedFiles.length > 0 ? ` (${selectedFiles.length})` : ''}`}
            </button>
          </div>
        </div>
    </>
  )

  if (embedded) {
    return inner
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h2>发送文件</h2>
          <button className="close-button" onClick={onClose}>
            ×
          </button>
        </div>
        {inner}
      </div>
    </div>
  )
}

export default FileSender
