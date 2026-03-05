import { useState, useEffect } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './InviteDialog.css'

interface FriendInfo {
  device_id: string
  instance_name: string
  online: boolean
  transfer_addr: string | null
}

interface Props {
  onClose: () => void
}

function InviteDialog({ onClose }: Props) {
  const [tab, setTab] = useState<'generate' | 'accept'>('generate')
  const [inviteCode, setInviteCode] = useState<string>('')
  const [codeError, setCodeError] = useState<string>('')
  const [copied, setCopied] = useState(false)
  const [acceptInput, setAcceptInput] = useState('')
  const [acceptError, setAcceptError] = useState('')
  const [accepting, setAccepting] = useState(false)

  useEffect(() => {
    if (tab === 'generate') {
      setInviteCode('')
      setCodeError('')
      setCopied(false)
      invoke<string>('generate_invite_code').then((code) => {
        setInviteCode(code)
      }).catch((e) => {
        setCodeError(String(e))
      })
    }
  }, [tab])

  const handleCopy = () => {
    navigator.clipboard.writeText(inviteCode).then(() => {
      setCopied(true)
    })
  }

  const handleAccept = async () => {
    const code = acceptInput.trim()
    if (!code) return
    setAccepting(true)
    setAcceptError('')
    try {
      await invoke<FriendInfo>('accept_invite_code', { code })
      onClose()
    } catch (e) {
      setAcceptError(String(e))
    } finally {
      setAccepting(false)
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content invite-dialog" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h2>邀请好友</h2>
          <button className="close-button" onClick={onClose}>×</button>
        </div>
        <div className="invite-tabs">
          <button
            className={`invite-tab${tab === 'generate' ? ' active' : ''}`}
            onClick={() => setTab('generate')}
          >
            生成邀请码
          </button>
          <button
            className={`invite-tab${tab === 'accept' ? ' active' : ''}`}
            onClick={() => setTab('accept')}
          >
            输入邀请码
          </button>
        </div>
        <div className="modal-body">
          {tab === 'generate' && (
            <div className="invite-generate">
              {codeError ? (
                <p className="invite-error">{codeError}</p>
              ) : inviteCode ? (
                <>
                  <div className="invite-code-row">
                    <span className="invite-code-text">{inviteCode}</span>
                    <button className="invite-copy-btn" onClick={handleCopy}>
                      {copied ? '已复制' : '复制'}
                    </button>
                  </div>
                  <p className="invite-hint">将此邀请码发送给好友，邀请码有效期 10 分钟</p>
                </>
              ) : (
                <p className="invite-hint">生成中...</p>
              )}
            </div>
          )}
          {tab === 'accept' && (
            <div className="invite-accept">
              <input
                type="text"
                className="invite-input"
                placeholder="输入对方的邀请码"
                value={acceptInput}
                onChange={(e) => setAcceptInput(e.target.value)}
                onKeyDown={(e) => { if (e.key === 'Enter') handleAccept() }}
              />
              {acceptError && <p className="invite-error">{acceptError}</p>}
              <button
                className="invite-confirm-btn"
                onClick={handleAccept}
                disabled={accepting || !acceptInput.trim()}
              >
                {accepting ? '处理中...' : '确认'}
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

export default InviteDialog
