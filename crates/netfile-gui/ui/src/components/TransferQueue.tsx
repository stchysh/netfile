import { invoke } from '@tauri-apps/api/core'
import './TransferQueue.css'

interface Transfer {
  file_id: string
  file_name: string
  total_size: number
  transferred: number
  total_chunks: number
  completed_chunks: number
  speed: number
  eta_secs: number
  direction: string
}

interface Props {
  transfers: Transfer[]
}

function TransferQueue({ transfers }: Props) {
  const formatSize = (bytes: number): string => {
    if (bytes === 0) return '0 B'
    const k = 1024
    const sizes = ['B', 'KB', 'MB', 'GB']
    const i = Math.floor(Math.log(bytes) / Math.log(k))
    return `${(bytes / Math.pow(k, i)).toFixed(2)} ${sizes[i]}`
  }

  const formatSpeed = (bytesPerSec: number): string => {
    return `${formatSize(bytesPerSec)}/s`
  }

  const formatEta = (secs: number): string => {
    if (secs === 0) return ''
    if (secs >= 3600) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`
    if (secs >= 60) return `${Math.floor(secs / 60)}m ${secs % 60}s`
    return `${secs}s`
  }

  const calculateProgress = (transferred: number, total: number): number => {
    if (total === 0) return 0
    return Math.round((transferred / total) * 100)
  }

  const handleCancel = async (fileId: string) => {
    try {
      await invoke('cancel_transfer', { fileId })
    } catch (error) {
      console.error('Failed to cancel transfer:', error)
    }
  }

  return (
    <div className="transfer-queue">
      <div className="transfer-queue-header">
        <h2>传输队列</h2>
      </div>
      <div className="transfer-queue-content">
        {transfers.length === 0 ? (
          <div className="empty-state">
            <p>暂无传输任务</p>
            <p className="hint">选择设备并发送文件开始传输</p>
          </div>
        ) : (
          transfers.map((transfer) => {
            const progress = calculateProgress(transfer.transferred, transfer.total_size)
            const eta = formatEta(transfer.eta_secs)
            return (
              <div key={transfer.file_id} className="transfer-item">
                <div className="transfer-header">
                  <div className="transfer-name-row">
                    <span className={`direction-badge direction-${transfer.direction}`}>
                      {transfer.direction === 'send' ? '发送' : '接收'}
                    </span>
                    <div className="transfer-name">{transfer.file_name}</div>
                  </div>
                  <div className="transfer-header-right">
                    <div className="transfer-progress-text">{progress}%</div>
                    <button
                      className="transfer-cancel-button"
                      onClick={() => handleCancel(transfer.file_id)}
                      title="取消传输"
                    >
                      ×
                    </button>
                  </div>
                </div>
                <div className="progress-bar">
                  <div
                    className="progress-fill"
                    style={{ width: `${progress}%` }}
                  ></div>
                </div>
                <div className="transfer-stats">
                  <span className="transfer-size">
                    {formatSize(transfer.transferred)} / {formatSize(transfer.total_size)}
                  </span>
                  <span className="transfer-meta">
                    {transfer.speed > 0 && (
                      <span className="transfer-speed">{formatSpeed(transfer.speed)}</span>
                    )}
                    {eta && (
                      <span className="transfer-eta">剩余 {eta}</span>
                    )}
                  </span>
                </div>
              </div>
            )
          })
        )}
      </div>
    </div>
  )
}

export default TransferQueue
