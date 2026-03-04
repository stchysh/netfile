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
  elapsed_secs: number
  direction: string
  status: string
  paused: boolean
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

  const formatElapsed = (secs: number): string => {
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

  const handlePause = async (fileId: string) => {
    try {
      await invoke('pause_transfer', { fileId })
    } catch (error) {
      console.error('Failed to pause transfer:', error)
    }
  }

  const handleResume = async (fileId: string) => {
    try {
      await invoke('resume_transfer', { fileId })
    } catch (error) {
      console.error('Failed to resume transfer:', error)
    }
  }

  const handleConfirm = async (fileId: string) => {
    try {
      await invoke('confirm_transfer', { fileId })
    } catch (error) {
      console.error('Failed to confirm transfer:', error)
    }
  }

  const handleReject = async (fileId: string) => {
    try {
      await invoke('reject_transfer', { fileId })
    } catch (error) {
      console.error('Failed to reject transfer:', error)
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
            if (transfer.status === 'pending_confirm') {
              return (
                <div key={transfer.file_id} className="transfer-item">
                  <div className="transfer-header">
                    <div className="transfer-name-row">
                      <span className="direction-badge direction-receive">接收</span>
                      <div className="transfer-name">{transfer.file_name}</div>
                    </div>
                    <div className="transfer-header-right">
                      <span className="transfer-pending-label">待确认</span>
                    </div>
                  </div>
                  <div className="transfer-stats">
                    <span className="transfer-size">{formatSize(transfer.total_size)}</span>
                  </div>
                  <div className="transfer-confirm-buttons">
                    <button
                      className="transfer-confirm-button"
                      onClick={() => handleConfirm(transfer.file_id)}
                    >
                      接收
                    </button>
                    <button
                      className="transfer-reject-button"
                      onClick={() => handleReject(transfer.file_id)}
                    >
                      拒绝
                    </button>
                  </div>
                </div>
              )
            }

            if (transfer.status === 'queued') {
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
                      <span className="transfer-queued-label">等待中</span>
                      <button
                        className="transfer-cancel-button"
                        onClick={() => handleCancel(transfer.file_id)}
                        title="取消传输"
                      >
                        ×
                      </button>
                    </div>
                  </div>
                  <div className="transfer-stats">
                    <span className="transfer-size">{formatSize(transfer.total_size)}</span>
                  </div>
                </div>
              )
            }

            const progress = calculateProgress(transfer.transferred, transfer.total_size)
            const eta = formatEta(transfer.eta_secs)
            const elapsed = formatElapsed(transfer.elapsed_secs)
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
                    {transfer.paused ? (
                      <span className="transfer-paused-label">已暂停</span>
                    ) : (
                      <div className="transfer-progress-text">{progress}%</div>
                    )}
                    {transfer.direction === 'send' && (
                      transfer.paused ? (
                        <button
                          className="transfer-action-button"
                          onClick={() => handleResume(transfer.file_id)}
                          title="继续传输"
                        >
                          继续
                        </button>
                      ) : (
                        <button
                          className="transfer-action-button"
                          onClick={() => handlePause(transfer.file_id)}
                          title="暂停传输"
                        >
                          暂停
                        </button>
                      )
                    )}
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
                    className={`progress-fill ${transfer.paused ? 'progress-fill-paused' : ''}`}
                    style={{ width: `${progress}%` }}
                  ></div>
                </div>
                <div className="transfer-stats">
                  <span className="transfer-size">
                    {formatSize(transfer.transferred)} / {formatSize(transfer.total_size)}
                  </span>
                  <span className="transfer-meta">
                    {!transfer.paused && transfer.speed > 0 && (
                      <span className="transfer-speed">{formatSpeed(transfer.speed)}</span>
                    )}
                    {!transfer.paused && eta && (
                      <span className="transfer-eta">剩余 {eta}</span>
                    )}
                    {elapsed && (
                      <span className="transfer-elapsed">已用 {elapsed}</span>
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
