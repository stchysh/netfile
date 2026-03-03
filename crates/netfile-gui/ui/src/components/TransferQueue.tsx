import './TransferQueue.css'

interface Transfer {
  file_id: string
  file_name: string
  total_size: number
  transferred: number
  speed: number
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

  const calculateProgress = (transferred: number, total: number): number => {
    if (total === 0) return 0
    return Math.round((transferred / total) * 100)
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
            return (
              <div key={transfer.file_id} className="transfer-item">
                <div className="transfer-header">
                  <div className="transfer-name">{transfer.file_name}</div>
                  <div className="transfer-progress-text">{progress}%</div>
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
                  <span className="transfer-speed">
                    {formatSpeed(transfer.speed)}
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
