import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
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
}

interface SelectedFile {
  path: string
  name: string
  size: number
}

function FileSender({ device, onClose }: Props) {
  const [selectedFiles, setSelectedFiles] = useState<SelectedFile[]>([])
  const [enableCompression, setEnableCompression] = useState(false)
  const [sending, setSending] = useState(false)
  const [dragOver, setDragOver] = useState(false)

  if (!device) return null

  const handleSelectFile = async () => {
    try {
      const selected = await open({
        multiple: true,
        directory: false,
      })

      if (selected) {
        const files = Array.isArray(selected) ? selected : [selected]
        const newFiles: SelectedFile[] = []

        for (const path of files) {
          const name = path.split(/[\\/]/).pop() || path
          newFiles.push({
            path,
            name,
            size: 0,
          })
        }

        setSelectedFiles([...selectedFiles, ...newFiles])
      }
    } catch (error) {
      console.error('Failed to select file:', error)
    }
  }

  const handleSelectFolder = async () => {
    try {
      const selected = await open({
        multiple: false,
        directory: true,
      })

      if (selected) {
        const name = selected.split(/[\\/]/).pop() || selected
        setSelectedFiles([
          ...selectedFiles,
          {
            path: selected,
            name: `${name}/`,
            size: 0,
          },
        ])
      }
    } catch (error) {
      console.error('Failed to select folder:', error)
    }
  }

  const handleRemoveFile = (index: number) => {
    setSelectedFiles(selectedFiles.filter((_, i) => i !== index))
  }

  const handleSend = async () => {
    if (selectedFiles.length === 0) {
      alert('请先选择文件')
      return
    }

    setSending(true)
    const targetAddr = `${device.ip}:${device.port}`

    try {
      for (const file of selectedFiles) {
        await invoke('send_file', {
          targetAddr,
          filePath: file.path,
        })
      }
      alert('文件发送成功！')
      onClose()
    } catch (error) {
      console.error('Failed to send files:', error)
      alert(`发送失败: ${error}`)
    } finally {
      setSending(false)
    }
  }

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(true)
  }

  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(false)
  }

  const handleDrop = async (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(false)

    // Note: File drop handling in Tauri requires additional setup
    // This is a placeholder for the UI
    alert('拖拽功能需要额外配置，请使用"添加文件"按钮')
  }

  const formatSize = (bytes: number): string => {
    if (bytes === 0) return '未知大小'
    const k = 1024
    const sizes = ['B', 'KB', 'MB', 'GB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return `${(bytes / Math.pow(k, i)).toFixed(2)} ${sizes[i]}`
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
                        {file.size > 0 && (
                          <div className="file-size">{formatSize(file.size)}</div>
                        )}
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
          <button className="cancel-button" onClick={onClose} disabled={sending}>
            取消
          </button>
          <button
            className="send-button"
            onClick={handleSend}
            disabled={sending || selectedFiles.length === 0}
          >
            {sending ? '发送中...' : '发送'}
          </button>
        </div>
      </div>
    </div>
  )
}

export default FileSender
