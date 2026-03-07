import { useState, useEffect, useRef } from 'react'
import { invoke } from '@tauri-apps/api/core'
import './Chat.css'

interface Device {
  device_id: string
  instance_id: string
  device_name: string
  instance_name: string
  ip: string
  port: number
}

interface ChatMessage {
  id: string
  from_instance_id: string
  from_instance_name: string
  content: string
  timestamp: number
  local_seq: number
  is_self: boolean
}

interface ConversationDelta {
  messages: ChatMessage[]
  next_cursor: number
  reset: boolean
}

interface Props {
  device: Device
}

const EMOJIS = [
  '😊','😂','😭','😅','🤣','😍','🥰','😘','😎','😏',
  '😒','🙄','😤','😡','🤔','😴','🥳','😱','🤩','💀',
  '👀','🫡','🤭','😇','🫶','❤️','👍','👎','🙏','💪',
  '🎉','🔥','💯','✅','👋','🤦','🙈','😆','😋','🤗',
  '🤫','🫠','💔','😢','😔','🥲','🤷','🫣','😼','🐶',
]

const normalizeTimestamp = (timestamp: number): number =>
  timestamp < 1_000_000_000_000 ? timestamp * 1000 : timestamp

const sortMessages = (messages: ChatMessage[]): ChatMessage[] =>
  [...messages].sort((a, b) => {
    const seqDiff = (a.local_seq ?? 0) - (b.local_seq ?? 0)
    if (seqDiff !== 0) return seqDiff

    const tsDiff = normalizeTimestamp(a.timestamp) - normalizeTimestamp(b.timestamp)
    if (tsDiff !== 0) return tsDiff

    return a.id.localeCompare(b.id)
  })

function Chat({ device }: Props) {
  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [input, setInput] = useState('')
  const [showEmojis, setShowEmojis] = useState(false)
  const cursorRef = useRef(0)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    let cancelled = false

    const applyDelta = (delta: ConversationDelta, replace: boolean) => {
      cursorRef.current = delta.next_cursor
      setMessages(prev => {
        if (replace || delta.reset) return sortMessages(delta.messages)
        if (delta.messages.length === 0) return prev
        return sortMessages([...prev, ...delta.messages])
      })
    }

    const load = async (replace = false) => {
      try {
        const delta = await invoke<ConversationDelta>('get_conversation_delta', {
          peerInstanceId: device.device_id,
          cursor: replace ? 0 : cursorRef.current,
        })
        if (!cancelled) applyDelta(delta, replace)
      } catch (error) {
        console.error('Failed to load conversation:', error)
      }
    }

    cursorRef.current = 0
    setMessages([])
    load(true)
    const interval = setInterval(load, 1000)
    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [device.device_id])

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'auto' })
  }, [messages])

  const formatTime = (timestamp: number): string => {
    const date = new Date(normalizeTimestamp(timestamp))
    const h = date.getHours().toString().padStart(2, '0')
    const m = date.getMinutes().toString().padStart(2, '0')
    return `${h}:${m}`
  }

  const handleSend = async () => {
    const content = input.trim()
    if (!content) return
    setInput('')
    setShowEmojis(false)
    const targetAddr = `${device.ip}:${device.port}`
    try {
      await invoke('send_text_message', {
        peerInstanceId: device.device_id,
        targetAddr,
        content,
      })
      const delta = await invoke<ConversationDelta>('get_conversation_delta', {
        peerInstanceId: device.device_id,
        cursor: cursorRef.current,
      })
      cursorRef.current = delta.next_cursor
      setMessages(prev => {
        if (delta.reset) return sortMessages(delta.messages)
        if (delta.messages.length === 0) return prev
        return sortMessages([...prev, ...delta.messages])
      })
    } catch (error) {
      console.error('Failed to send message:', error)
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSend()
    }
    if (e.key === 'Escape') {
      setShowEmojis(false)
    }
  }

  const handleEmojiClick = (emoji: string) => {
    const el = inputRef.current
    if (el) {
      const start = el.selectionStart ?? input.length
      const end = el.selectionEnd ?? input.length
      const next = input.slice(0, start) + emoji + input.slice(end)
      setInput(next)
      setTimeout(() => {
        el.focus()
        el.setSelectionRange(start + emoji.length, start + emoji.length)
      }, 0)
    } else {
      setInput(input + emoji)
    }
  }

  return (
    <div className="chat-container">
      <div className="chat-messages">
        {messages.map((msg) => (
          <div key={msg.id} className={`chat-bubble-row ${msg.is_self ? 'self' : 'other'}`}>
            {!msg.is_self && (
              <div className="chat-sender-name">{msg.from_instance_name}</div>
            )}
            <div className={`chat-bubble ${msg.is_self ? 'bubble-self' : 'bubble-other'}`}>
              <span className="bubble-content">{msg.content}</span>
              <span className="bubble-time">{formatTime(msg.timestamp)}</span>
            </div>
          </div>
        ))}
        <div ref={messagesEndRef} />
      </div>
      <div className="chat-input-area">
        {showEmojis && (
          <div className="emoji-panel">
            {EMOJIS.map((emoji) => (
              <button
                key={emoji}
                className="emoji-btn"
                onClick={() => handleEmojiClick(emoji)}
              >
                {emoji}
              </button>
            ))}
          </div>
        )}
        <div className="chat-input-row">
          <button
            className={`emoji-toggle-btn ${showEmojis ? 'active' : ''}`}
            onClick={() => setShowEmojis(!showEmojis)}
            title="表情"
          >
            😊
          </button>
          <input
            ref={inputRef}
            className="chat-input"
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="输入消息..."
          />
          <button className="chat-send-button" onClick={handleSend}>
            发送
          </button>
        </div>
      </div>
    </div>
  )
}

export default Chat
