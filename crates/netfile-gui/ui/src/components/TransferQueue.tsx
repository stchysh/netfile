import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
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
  current_file?: string
  error?: string
  transfer_method?: string
  speed_limit_source?: string
}

function methodLabel(method?: string): string {
  if (method === 'lan') return 'LAN'
  if (method === 'iroh') return 'NAT'
  return ''
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

  const hasPendingConfirm = transfers.some(t => t.status === 'pending_confirm')
  const hasActiveSend = transfers.some(t => t.direction === 'send' && t.status === 'active' && !t.paused)
  const hasPaused = transfers.some(t => t.paused)

  const handleConfirmAll = async () => {
    const pending = transfers.filter(t => t.status === 'pending_confirm')
    for (const t of pending) {
      try {
        await invoke('confirm_transfer', { fileId: t.file_id })
      } catch (error) {
        console.error('Failed to confirm transfer:', error)
      }
    }
  }

  const handleRejectAll = async () => {
    const pending = transfers.filter(t => t.status === 'pending_confirm')
    for (const t of pending) {
      try {
        await invoke('reject_transfer', { fileId: t.file_id })
      } catch (error) {
        console.error('Failed to reject transfer:', error)
      }
    }
  }

  const handlePauseAll = async () => {
    try {
      await invoke('pause_all_transfers')
    } catch (error) {
      console.error('Failed to pause all:', error)
    }
  }

  const handleResumeAll = async () => {
    try {
      await invoke('resume_all_transfers')
    } catch (error) {
      console.error('Failed to resume all:', error)
    }
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

  const handleSaveAs = async (fileId: string) => {
    try {
      const selected = await open({ directory: true, multiple: false, title: '选择保存目录' })
      if (selected) {
        await invoke('confirm_transfer_save_as', { fileId, savePath: selected })
      }
    } catch (error) {
      console.error('Failed to save as:', error)
    }
  }

  return (
    <div className="transfer-queue">
      <div className="transfer-queue-header">
        <h2>传输队列</h2>
        <div className="transfer-queue-actions">
          {hasPendingConfirm && (
            <button className="queue-action-button queue-action-confirm" onClick={handleConfirmAll}>
              全部接收
            </button>
          )}
          {hasPendingConfirm && (
            <button className="queue-action-button queue-action-reject" onClick={handleRejectAll}>
              全部拒绝
            </button>
          )}
          {hasActiveSend && (
            <button className="queue-action-button" onClick={handlePauseAll}>
              全部暂停
            </button>
          )}
          {hasPaused && (
            <button className="queue-action-button" onClick={handleResumeAll}>
              全部继续
            </button>
          )}
        </div>
      </div>
      <div className="transfer-queue-content">
        {transfers.length === 0 ? (
          <div className="empty-state">
            <p>暂无传输任务</p>
            <p className="hint">选择设备并发送文件开始传输</p>
          </div>
        ) : (
          (() => {
            const pendingConfirm = transfers.filter(t => t.status === 'pending_confirm')
            const active = transfers.filter(t => t.status !== 'pending_confirm' && t.status !== 'queued')
            const queued = hasPendingConfirm ? [] : transfers.filter(t => t.status === 'queued')
            const sorted = [...pendingConfirm, ...active, ...queued]
            return sorted.map((transfer) => {
            if (transfer.status === 'pending_confirm') {
              const mLabel = methodLabel(transfer.transfer_method)
              return (
                <div key={transfer.file_id} className="transfer-item">
                  <div className="transfer-header">
                    <div className="transfer-name-row">
                      <span className="direction-badge direction-receive">接收</span>
                      {mLabel && <span className="method-badge">{mLabel}</span>}
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
                      className="transfer-saveas-button"
                      onClick={() => handleSaveAs(transfer.file_id)}
                    >
                      另存为
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

            if (transfer.status === 'error') {
              return (
                <div key={transfer.file_id} className="transfer-item transfer-item-error">
                  <div className="transfer-header">
                    <div className="transfer-name-row">
                      <span className={`direction-badge direction-${transfer.direction}`}>
                        {transfer.direction === 'send' ? '发送' : '接收'}
                      </span>
                      <div className="transfer-name">{transfer.file_name}</div>
                    </div>
                    <div className="transfer-header-right">
                      <span className="transfer-error-label">失败</span>
                      <button
                        className="transfer-cancel-button"
                        onClick={() => handleCancel(transfer.file_id)}
                        title="关闭"
                      >
                        ×
                      </button>
                    </div>
                  </div>
                  {transfer.error && (
                    <div className="transfer-error-msg">{transfer.error}</div>
                  )}
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
            const mLabel = methodLabel(transfer.transfer_method)
            return (
              <div key={transfer.file_id} className="transfer-item">
                <div className="transfer-header">
                  <div className="transfer-name-row">
                    <span className={`direction-badge direction-${transfer.direction}`}>
                      {transfer.direction === 'send' ? '发送' : '接收'}
                    </span>
                    {mLabel && <span className="method-badge">{mLabel}</span>}
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
                {transfer.current_file && (
                  <div className="transfer-current-file">{transfer.current_file}</div>
                )}
                <div className="transfer-stats">
                  <span className="transfer-size">
                    {formatSize(transfer.transferred)} / {formatSize(transfer.total_size)}
                  </span>
                  <span className="transfer-meta">
                    {!transfer.paused && transfer.speed > 0 && (
                      <span className="transfer-speed">{formatSpeed(transfer.speed)}</span>
                    )}
                    {!transfer.paused && transfer.speed_limit_source && (
                      <span className="transfer-limit-source">
                        {transfer.speed_limit_source === 'sender' ? '发送端限速' : '接收端限速'}
                      </span>
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
          })()
        )}
      </div>
    </div>
  )
}

export default TransferQueue
